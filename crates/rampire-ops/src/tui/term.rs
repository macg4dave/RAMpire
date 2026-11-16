//! The terminal the TUI draws on: opening and giving it back, each frame
//! drawn as one synchronized update so a slow SSH link never shows half a
//! frame, Ctrl+Z, copying with OSC 52 (which reaches the clipboard of the
//! computer the person sits at, through SSH and sudo), and asking the
//! terminal what it can show before anything is drawn.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::{DefaultTerminal, Frame};

/// The terminal, open in the alternate screen.
pub struct Term {
    terminal: DefaultTerminal,
}

impl Term {
    pub fn open() -> Result<Self, String> {
        let terminal =
            ratatui::try_init().map_err(|error| format!("open the terminal: {error}"))?;
        Ok(Self { terminal })
    }

    /// Draw a frame as one update: a terminal that knows synchronized
    /// output shows it whole; others ignore the markers.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame)) -> Result<(), String> {
        let mut stdout = std::io::stdout();
        execute!(stdout, BeginSynchronizedUpdate).map_err(|error| format!("draw: {error}"))?;
        let drawn = self
            .terminal
            .draw(render)
            .map(|_| ())
            .map_err(|error| format!("draw: {error}"));
        // The end marker goes out whether or not the frame did.
        let ended =
            execute!(stdout, EndSynchronizedUpdate).map_err(|error| format!("draw: {error}"));
        drawn.and(ended)
    }

    /// Ctrl+Z: give the terminal back and stop, as a shell's job; on `fg`,
    /// take it again and draw everything anew.
    pub fn suspend(&mut self) -> Result<(), String> {
        let mut stdout = std::io::stdout();
        disable_raw_mode().map_err(|error| format!("suspend: {error}"))?;
        execute!(stdout, LeaveAlternateScreen, Show)
            .map_err(|error| format!("suspend: {error}"))?;
        signal_hook::low_level::raise(signal_hook::consts::SIGTSTP)
            .map_err(|error| format!("suspend: {error}"))?;
        // Running again.
        enable_raw_mode().map_err(|error| format!("resume: {error}"))?;
        execute!(stdout, EnterAlternateScreen, Hide).map_err(|error| format!("resume: {error}"))?;
        self.terminal
            .clear()
            .map_err(|error| format!("resume: {error}"))
    }

    /// Give the terminal back as it was. A terminal that has gone away (a
    /// closed window, a dropped SSH connection) can't be given back, and
    /// there is nothing to give it back to: that isn't a failure.
    pub fn close(self) -> Result<(), String> {
        drop(self.terminal);
        match ratatui::try_restore() {
            Ok(()) => Ok(()),
            Err(_) if hung_up() => Ok(()),
            Err(error) => Err(format!("restore the terminal: {error}")),
        }
    }
}

/// Put `text` on the clipboard of the person's own computer: OSC 52, which
/// terminals pass from any program however far away, and through tmux or
/// screen when they allow it.
pub fn copy(text: &str) -> Result<(), String> {
    let sequence = osc52(text, &std::env::var("TERM").unwrap_or_default());
    let mut stdout = std::io::stdout();
    stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("copy: {error}"))
}

/// The OSC 52 sequence for `text`; under tmux or screen also wrapped for
/// it to pass through.
fn osc52(text: &str, term: &str) -> String {
    let plain = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    if term.starts_with("tmux") || term.starts_with("screen") {
        let wrapped = format!("\x1bPtmux;{}\x1b\\", plain.replace('\x1b', "\x1b\x1b"));
        format!("{plain}{wrapped}")
    } else {
        plain
    }
}

/// Standard base64, padded, as OSC 52 takes it.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
            value | u32::from(*byte) << (16 - 8 * index)
        });
        for place in 0..4 {
            if place <= chunk.len() {
                let index = (triple >> (18 - 6 * place)) & 0x3f;
                text.push(char::from(ALPHABET[index as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// What the terminal said about itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Probe {
    /// It kept a 24-bit colour when asked to; `None` when it didn't say.
    pub true_colour: Option<bool>,
    /// Its background is light; `None` when it didn't say.
    pub light: Option<bool>,
}

/// The questions, in order: the background colour (OSC 11), the colour
/// just set (a 24-bit one, then asked back with DECRQSS), and last the
/// device attributes (DA1), which every terminal answers, so its answer
/// marks the end of the others'.
const QUESTIONS: &str = "\x1b]11;?\x1b\\\x1b[48;2;1;2;3m\x1bP$qm\x1b\\\x1b[m\x1b[c";

/// How long to wait for the terminal's answers: long enough for a slow SSH
/// link; a terminal answers in milliseconds.
const PROBE_WAIT: Duration = Duration::from_secs(1);

/// Ask the terminal on stdin and stdout what it can show, before the TUI
/// opens it. Nothing is asked of a terminal that isn't one.
pub fn probe() -> Probe {
    let Ok(mut tty) = File::options().read(true).write(true).open("/dev/tty") else {
        return Probe::default();
    };
    if enable_raw_mode().is_err() {
        return Probe::default();
    }
    let answers = ask(&mut tty);
    // Raw mode off again whatever was read; a failure here leaves the
    // terminal as the TUI is about to set it anyway.
    if let Err(error) = disable_raw_mode() {
        eprintln!("rampire: warning: {error}");
    }
    parse_probe(&answers)
}

fn ask(tty: &mut File) -> Vec<u8> {
    let mut answers = Vec::new();
    if tty
        .write_all(QUESTIONS.as_bytes())
        .and_then(|()| tty.flush())
        .is_err()
    {
        return answers;
    }
    let started = Instant::now();
    let mut buffer = [0u8; 256];
    while started.elapsed() < PROBE_WAIT && !answered(&answers) {
        let left = PROBE_WAIT.saturating_sub(started.elapsed());
        if !readable(tty.as_raw_fd(), left) {
            break;
        }
        match tty.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => answers.extend_from_slice(&buffer[..read]),
        }
    }
    answers
}

/// Whether the device attributes, the last answer, have come.
fn answered(answers: &[u8]) -> bool {
    let text = String::from_utf8_lossy(answers);
    text.find("\x1b[?")
        .is_some_and(|start| text[start..].contains('c'))
}

/// What the answers say.
fn parse_probe(answers: &[u8]) -> Probe {
    let text = String::from_utf8_lossy(answers);
    let light = text
        .find("\x1b]11;rgb:")
        .and_then(|start| background(&text[start + 9..]))
        .map(|(red, green, blue)| 0.2126 * red + 0.7152 * green + 0.0722 * blue > 0.5);
    let true_colour = text.find("\x1bP").and_then(|start| {
        let reply = &text[start..];
        if reply.starts_with("\x1bP1$r") {
            let end = reply.find("\x1b\\").unwrap_or(reply.len());
            let sgr = &reply[..end];
            Some(sgr.contains("1:2:3") || sgr.contains("1;2;3"))
        } else {
            None
        }
    });
    Probe { true_colour, light }
}

/// `RRRR/GGGG/BBBB` as fractions of full.
fn background(text: &str) -> Option<(f64, f64, f64)> {
    let end = text.find(['\x07', '\x1b']).unwrap_or(text.len());
    let mut parts = text[..end].split('/').map(|part| {
        let value = u32::from_str_radix(part, 16).ok()?;
        let full = (1u64 << (4 * part.len().clamp(1, 4))) - 1;
        Some(f64::from(value) / full as f64)
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Whether `fd` has something to read within `wait`.
fn readable(fd: i32, wait: Duration) -> bool {
    let mut descriptor = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let timeout = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one caller-owned pollfd, for the duration of the call.
    let ready = unsafe { poll(&mut descriptor, 1, timeout) };
    ready > 0 && descriptor.revents & POLLIN != 0
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
}

const POLLIN: i16 = 0x1;
const POLLERR: i16 = 0x8;
const POLLHUP: i16 = 0x10;
const POLLNVAL: i16 = 0x20;

/// Whether the terminal on stdin has hung up or gone.
pub fn hung_up() -> bool {
    let mut descriptor = PollFd {
        fd: 0,
        events: 0,
        revents: 0,
    };
    // SAFETY: one caller-owned pollfd, and a zero timeout.
    let ready = unsafe { poll(&mut descriptor, 1, 0) };
    ready < 0 || descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0
}

/// While it lives, what is written to stderr (fd 2) goes to `tx` as
/// [`super::jobs::Msg::Stderr`], a line at a time, instead of over the
/// screen: a library's warning or a program that inherited stderr. Dropped,
/// stderr is the terminal again, so a failure after the screen closes is
/// printed as usual.
pub struct StderrToErrors {
    saved: libc::c_int,
}

impl StderrToErrors {
    pub fn start(tx: std::sync::mpsc::Sender<super::jobs::Msg>) -> Result<Self, String> {
        use std::io::BufRead;
        use std::os::fd::FromRawFd;
        let mut ends = [0; 2];
        // SAFETY: pipe2 fills both descriptors or fails; dup and dup2 take
        // descriptors this process owns, and each is closed once.
        unsafe {
            if libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
                return Err(format!(
                    "pipe for stderr: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let saved = libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 3);
            if saved < 0 || libc::dup2(ends[1], libc::STDERR_FILENO) < 0 {
                let error = std::io::Error::last_os_error();
                libc::close(ends[0]);
                libc::close(ends[1]);
                if saved >= 0 {
                    libc::close(saved);
                }
                return Err(format!("send stderr to the errors list: {error}"));
            }
            libc::close(ends[1]);
            let reader = std::fs::File::from_raw_fd(ends[0]);
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    let line = line.trim().to_owned();
                    if !line.is_empty() && tx.send(super::jobs::Msg::Stderr(line)).is_err() {
                        break;
                    }
                }
            });
            Ok(Self { saved })
        }
    }
}

impl Drop for StderrToErrors {
    fn drop(&mut self) {
        // SAFETY: `saved` is the terminal's stderr, duplicated in start.
        // Once fd 2 is it again, the pipe's last writer is gone (programs
        // still running hold their own copy) and the reader ends.
        unsafe {
            libc::dup2(self.saved, libc::STDERR_FILENO);
            libc::close(self.saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_goes_to_the_errors_list_while_the_screen_is_open() {
        let (tx, rx) = std::sync::mpsc::channel();
        let capture = StderrToErrors::start(tx).unwrap();
        // As a library writes it: straight to fd 2, past Rust's capture.
        let text = b"libvirt: error : guest agent is not responding\n";
        // SAFETY: fd 2 is open; text is a valid buffer of its length.
        unsafe { libc::write(libc::STDERR_FILENO, text.as_ptr().cast(), text.len()) };
        let got = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        drop(capture);
        match got {
            super::super::jobs::Msg::Stderr(line) => {
                assert_eq!(line, "libvirt: error : guest agent is not responding")
            }
            _ => panic!("not a stderr line"),
        }
    }

    #[test]
    fn base64_is_the_standard_padded_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(
            base64(b"rampire vm restart web"),
            "cmFtcGlyZSB2bSByZXN0YXJ0IHdlYg=="
        );
    }

    #[test]
    fn a_copy_is_one_osc52_sequence_and_passes_through_tmux() {
        assert_eq!(osc52("foo", "xterm-256color"), "\x1b]52;c;Zm9v\x07");
        let tmux = osc52("foo", "tmux-256color");
        assert!(
            tmux.starts_with("\x1b]52;c;Zm9v\x07\x1bPtmux;\x1b\x1b]52"),
            "{tmux:?}"
        );
        assert!(tmux.ends_with("\x1b\\"));
    }

    #[test]
    fn the_terminals_answers_say_its_background_and_its_colours() {
        let dark = b"\x1b]11;rgb:0505/0303/0707\x1b\\\x1bP1$r0;48:2::1:2:3m\x1b\\\x1b[?62;22c";
        assert!(answered(dark));
        assert_eq!(
            parse_probe(dark),
            Probe {
                true_colour: Some(true),
                light: Some(false)
            }
        );
        // A light background, and the colour came back as one of 256.
        let light = b"\x1b]11;rgb:ffff/ffff/f0f0\x07\x1bP1$r0;48;5;16m\x1b\\\x1b[?6c";
        assert_eq!(
            parse_probe(light),
            Probe {
                true_colour: Some(false),
                light: Some(true)
            }
        );
        // The Linux console answers only the device attributes.
        let console = b"\x1b[?6c";
        assert_eq!(parse_probe(console), Probe::default());
        assert!(!answered(b"\x1b]11;rgb:0/0/0\x07"));
    }
}
