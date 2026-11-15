//! The TUI: one full-screen program on the KVM host, on its console or over
//! `ssh -t HOST sudo rampire`. It shows the pool and each VM, adds and
//! removes VMs, and runs the doctor, through the same flows as the
//! commands, each as a task in the background. `rampire top` and
//! `rampire watch` draw its dashboard and VM details alone ([`live`]).
//!
//! It is built as Yazi is built: one key table ([`keymap`]) that every
//! key, hint, help line and palette command comes from; layers that take
//! keys topmost first ([`router`]) and are drawn bottom first ([`views`]);
//! and a loop that sleeps until a key, a reading or a moving notice needs
//! the screen drawn. Every action names the command that does the same,
//! and is logged to the journal with the person and `via=tui`.
//!
//! | Module | Holds |
//! |---|---|
//! | [`app`] | The state, what is derived from it, and the messages |
//! | [`router`] | Where a key goes, and what each command does there |
//! | [`keymap`] | Every key, its command, description and hint |
//! | [`views`] | The screen: header, view, status bar, layers |
//! | [`tasks`] | The actions running in the background, and their questions |
//! | [`which`], [`help`], [`palette`], [`spot`], [`dialog`], [`notify`] | What opens over the view |
//! | [`list`], [`scroll`], [`input`] | The VM list's state, moving through lists, text fields |
//! | [`jobs`], [`actions`], [`peers`] | The work off the screen's thread |
//! | [`term`], [`theme`], [`widgets`] | The terminal, the colours and the drawing pieces |
//! | [`pool`], [`vm`], [`live`] | The dashboard and a VM's details, shared with `top` and `watch` |

pub mod actions;
pub mod app;
pub mod chart;
pub mod dialog;
pub mod form;
pub mod help;
pub mod input;
pub mod jobs;
pub mod keymap;
pub mod list;
pub mod live;
pub mod notify;
pub mod palette;
pub mod peers;
pub mod pool;
pub mod router;
pub mod scroll;
pub mod spot;
pub mod tasks;
pub mod term;
pub mod theme;
pub mod views;
pub mod vm;
pub mod which;
pub mod widgets;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::context::Context;
use app::Effect;
use theme::{Depth, Icons, Palette, Theme};

/// The look from the environment alone: midnight, at the
/// depth `TERM` names, with Unicode icons where the locale allows.
pub fn theme_from_environment() -> Theme {
    Theme::new(
        Palette::Midnight,
        Depth::from_environment(),
        Icons::from_environment(),
    )
}

/// The look for a screen about to open: as the environment says, then as
/// the terminal says when asked, what it can show and whether its
/// background is light. Only for a terminal the screen will take over.
pub fn theme_for_screen() -> Theme {
    let theme = theme_from_environment();
    if theme.depth == Depth::None {
        return theme;
    }
    let probe = term::probe();
    let mut theme = theme.on_light(probe.light.unwrap_or(false));
    theme.depth = theme.depth.probed(probe.true_colour);
    theme
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

/// The shortest time between two frames: a burst of keys or readings is
/// drawn once.
const FRAME: Duration = Duration::from_millis(16);
/// The longest sleep, so a signal or a hang-up is noticed soon.
const NAP: Duration = Duration::from_millis(250);

fn run_app(context: Context) -> Result<(), String> {
    let stop = stop_on_signals(&[signal_hook::consts::SIGHUP, signal_hook::consts::SIGTERM])?;
    // Asked before the screen opens, so the answers aren't read as keys.
    let theme = theme_for_screen();
    let (tx, rx) = jobs::channel();
    let control = jobs::ReaderControl::new();
    jobs::spawn_reader(
        context.clone(),
        control.clone(),
        tx.clone(),
        Duration::from_secs(1),
    );
    let mut app = app::App::new(context, theme, tx.clone(), control.clone());
    let mut screen = term::Term::open()?;
    jobs::spawn_keys(tx);
    let result = (|| -> Result<(), String> {
        let mut dirty = true;
        let mut drawn = Instant::now() - FRAME;
        while !app.quit && !stop.load(Ordering::Acquire) {
            // A terminal that has gone away (a dropped SSH connection
            // under sudo's own pty sends no SIGHUP) ends the TUI.
            if term::hung_up() {
                return Ok(());
            }
            let now = Instant::now();
            if dirty && now >= drawn + FRAME {
                screen.draw(|frame| views::draw(frame, &app))?;
                drawn = now;
                dirty = false;
            }
            // Asleep until something comes, or the screen changes by
            // itself (a notice moving, the clock), or a frame is due.
            let deadline = if dirty {
                drawn + FRAME
            } else {
                app.next_wake(now)
            };
            let wait = deadline.saturating_duration_since(now).min(NAP);
            match rx.recv_timeout(wait) {
                Ok(msg) => {
                    app.on_msg(msg);
                    while let Ok(msg) = rx.try_recv() {
                        app.on_msg(msg);
                    }
                    dirty = true;
                }
                Err(RecvTimeoutError::Timeout) => dirty |= Instant::now() >= deadline,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            app.tick(Instant::now());
            for effect in std::mem::take(&mut app.effects) {
                match effect {
                    Effect::Copy(text) => term::copy(&text)?,
                    Effect::Suspend => screen.suspend()?,
                }
                dirty = true;
            }
        }
        Ok(())
    })();
    control.stop.store(true, Ordering::Release);
    screen.close()?;
    result
}
