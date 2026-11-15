//! How a guided flow (`vm add`, `vm remove`, `apply` and the rest) talks to
//! the person running it: progress lines, a change to look at, and
//! questions. The commands answer through [`Terminal`]; the TUI answers in
//! its dialogs. Nothing in a flow reads stdin or writes stdout itself.

use std::io::{BufRead, Write};

/// A person's request to stop a flow, checked at its safe points: the
/// waits for a VM to boot, a login to work, or a guest to report in. A
/// step that must finish once begun, such as shutting a VM down and
/// starting it again, or installing a package, is never stopped half-way.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Option<std::sync::Arc<std::sync::atomic::AtomicBool>>);

impl Cancel {
    /// One that is never set: for the commands, which Ctrl+C stops.
    pub fn never() -> Self {
        Self(None)
    }

    /// One a person can set from elsewhere, with [`Cancel::set`].
    pub fn new() -> Self {
        Self(Some(std::sync::Arc::default()))
    }

    pub fn set(&self) {
        if let Some(flag) = &self.0 {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    pub fn is_set(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
    }

    /// An error when it is set, saying what was being waited for.
    pub fn check(&self, waiting_for: &str) -> Result<(), String> {
        if self.is_set() {
            Err(format!("cancelled while waiting for {waiting_for}"))
        } else {
            Ok(())
        }
    }

    /// Sleep for `time`, waking early when it is set; whether it is set.
    pub fn sleep(&self, time: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + time;
        while std::time::Instant::now() < until {
            if self.is_set() {
                return true;
            }
            std::thread::sleep(
                until
                    .saturating_duration_since(std::time::Instant::now())
                    .min(std::time::Duration::from_millis(200)),
            );
        }
        self.is_set()
    }
}

/// The person on the other side of a flow.
pub trait Ui {
    /// Whether the person has asked the flow to stop, for its waits.
    fn cancel(&self) -> Cancel {
        Cancel::never()
    }

    /// One line of progress or result.
    fn say(&mut self, line: &str);
    /// A change to look at before a question, such as a diff, shown as it is.
    fn show(&mut self, text: &str);
    /// A yes-or-no question that `--yes` may answer for a script. `false`
    /// is no: the flow changes nothing more.
    fn confirm(&mut self, question: &str) -> Result<bool, String>;
    /// A yes-or-no question only a person may answer, such as a review
    /// question about the guest's programs: never assumed.
    fn review(&mut self, question: &str) -> Result<bool, String>;
    /// A short text only a person may give, such as a kernel release.
    fn text(&mut self, question: &str) -> Result<String, String>;
    /// A value with a suggested one a person may change: the suggestion is
    /// kept when there is no one to ask, with `--yes`, or when the answer
    /// is empty.
    fn edit(&mut self, question: &str, suggested: &str) -> Result<String, String> {
        let _ = question;
        Ok(suggested.to_owned())
    }
}

/// Whether a typed answer is yes.
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
}

/// The commands' side: lines on stdout, answers read from stdin.
pub struct Terminal<R, W> {
    input: R,
    output: W,
    /// Stdin is a terminal a person can answer on.
    interactive: bool,
    /// `--yes`: every [`Ui::confirm`] is answered yes.
    yes: bool,
}

impl Terminal<std::io::StdinLock<'static>, std::io::Stdout> {
    /// The process's own terminal, as the commands use it.
    pub fn stdio(yes: bool) -> Self {
        use std::io::IsTerminal;
        Self::new(
            std::io::stdin().lock(),
            std::io::stdout(),
            std::io::stdin().is_terminal(),
            yes,
        )
    }
}

impl<R: BufRead, W: Write> Terminal<R, W> {
    pub fn new(input: R, output: W, interactive: bool, yes: bool) -> Self {
        Self {
            input,
            output,
            interactive,
            yes,
        }
    }

    fn read_answer(&mut self, prompt: &str) -> Result<String, String> {
        write!(self.output, "{prompt}")
            .and_then(|()| self.output.flush())
            .map_err(|error| error.to_string())?;
        let mut answer = String::new();
        self.input
            .read_line(&mut answer)
            .map_err(|error| error.to_string())?;
        Ok(answer.trim().to_owned())
    }

    /// A question for a person, with no terminal to ask on.
    fn person_needed(question: &str) -> String {
        format!(
            "{question} needs a person: answer on a terminal, or name a reviewed profile with --review"
        )
    }

    pub fn into_parts(self) -> (R, W) {
        (self.input, self.output)
    }
}

impl<R: BufRead, W: Write> Ui for Terminal<R, W> {
    fn say(&mut self, line: &str) {
        let _ = writeln!(self.output, "{line}");
    }

    fn show(&mut self, text: &str) {
        let _ = write!(self.output, "{text}");
    }

    fn confirm(&mut self, question: &str) -> Result<bool, String> {
        if self.yes {
            return Ok(true);
        }
        if !self.interactive {
            return Err("no terminal to ask on: pass --yes to go ahead".to_owned());
        }
        Ok(is_yes(&self.read_answer(&format!("{question} [y/N] "))?))
    }

    fn review(&mut self, question: &str) -> Result<bool, String> {
        let question = format!("{question} [y/N]");
        if !self.interactive {
            return Err(Self::person_needed(&question));
        }
        Ok(is_yes(&self.read_answer(&format!("{question} "))?))
    }

    fn text(&mut self, question: &str) -> Result<String, String> {
        if !self.interactive {
            return Err(Self::person_needed(question));
        }
        self.read_answer(&format!("{question} "))
    }

    fn edit(&mut self, question: &str, suggested: &str) -> Result<String, String> {
        if self.yes || !self.interactive {
            return Ok(suggested.to_owned());
        }
        let answer = self.read_answer(&format!("{question} [{suggested}] "))?;
        Ok(if answer.is_empty() {
            suggested.to_owned()
        } else {
            answer
        })
    }
}

/// One thing a flow did, as [`Transcript`] records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    Line(String),
    Shown(String),
    Confirm(String, bool),
    Review(String, bool),
    Text(String, String),
}

/// A [`Ui`] that answers from a list and records everything, for tests and
/// for keeping what a flow said.
#[derive(Debug, Default)]
pub struct Transcript {
    /// Answers in order: "y", "n" or a text.
    pub answers: std::collections::VecDeque<String>,
    pub said: Vec<Said>,
}

impl Transcript {
    pub fn answering(answers: &[&str]) -> Self {
        Self {
            answers: answers.iter().map(|answer| (*answer).to_owned()).collect(),
            said: Vec::new(),
        }
    }

    fn next(&mut self, question: &str) -> Result<String, String> {
        self.answers
            .pop_front()
            .ok_or_else(|| format!("no answer left for: {question}"))
    }

    /// Every line and shown text, in order, as a terminal shows them.
    pub fn lines(&self) -> Vec<String> {
        self.said
            .iter()
            .filter_map(|said| match said {
                Said::Line(line) | Said::Shown(line) => Some(line.clone()),
                _ => None,
            })
            .collect()
    }
}

impl Ui for Transcript {
    fn say(&mut self, line: &str) {
        self.said.push(Said::Line(line.to_owned()));
    }

    fn show(&mut self, text: &str) {
        self.said.push(Said::Shown(text.to_owned()));
    }

    fn confirm(&mut self, question: &str) -> Result<bool, String> {
        let answer = is_yes(&self.next(question)?);
        self.said.push(Said::Confirm(question.to_owned(), answer));
        Ok(answer)
    }

    fn review(&mut self, question: &str) -> Result<bool, String> {
        let answer = is_yes(&self.next(question)?);
        self.said.push(Said::Review(question.to_owned(), answer));
        Ok(answer)
    }

    fn text(&mut self, question: &str) -> Result<String, String> {
        let answer = self.next(question)?;
        self.said
            .push(Said::Text(question.to_owned(), answer.clone()));
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(input: &str, interactive: bool, yes: bool) -> Terminal<&[u8], Vec<u8>> {
        Terminal::new(input.as_bytes(), Vec::new(), interactive, yes)
    }

    fn written(terminal: Terminal<&[u8], Vec<u8>>) -> String {
        String::from_utf8(terminal.into_parts().1).unwrap()
    }

    #[test]
    fn the_terminal_asks_as_the_commands_always_have() {
        let mut ui = terminal("y\n", true, false);
        assert!(ui
            .confirm("Apply this and reload rampire.service?")
            .unwrap());
        assert_eq!(written(ui), "Apply this and reload rampire.service? [y/N] ");

        let mut ui = terminal("\n", true, false);
        assert!(!ui.confirm("Apply this?").unwrap());

        let mut ui = terminal("yes\n", true, false);
        assert!(ui
            .review("Is no program in the guest pinned to memory?")
            .unwrap());
        assert_eq!(
            written(ui),
            "Is no program in the guest pinned to memory? [y/N] "
        );

        let mut ui = terminal("6.12.0\n", true, false);
        assert_eq!(ui.text("The guest's kernel release:").unwrap(), "6.12.0");
        assert_eq!(written(ui), "The guest's kernel release: ");

        let mut ui = terminal("", true, false);
        ui.say("changed /etc/rampire/vms.d/web.toml");
        ui.show("--- applied\n+++ draft\n");
        assert_eq!(
            written(ui),
            "changed /etc/rampire/vms.d/web.toml\n--- applied\n+++ draft\n"
        );
    }

    #[test]
    fn yes_answers_confirmations_but_never_a_persons_questions() {
        let mut ui = terminal("", false, true);
        assert!(ui.confirm("Apply this?").unwrap());
        assert_eq!(
            ui.review("Is it safe?").unwrap_err(),
            "Is it safe? [y/N] needs a person: answer on a terminal, or name a reviewed profile with --review"
        );
        assert!(ui.text("The guest's kernel release:").is_err());
        assert_eq!(written(ui), "");

        let mut ui = terminal("", false, false);
        assert_eq!(
            ui.confirm("Apply this?").unwrap_err(),
            "no terminal to ask on: pass --yes to go ahead"
        );
    }

    #[test]
    fn a_cancel_set_elsewhere_ends_a_wait_early() {
        assert!(!Cancel::never().sleep(std::time::Duration::from_millis(1)));
        let cancel = Cancel::new();
        let other = cancel.clone();
        assert!(cancel.check("the guest").is_ok());
        std::thread::spawn(move || other.set());
        let started = std::time::Instant::now();
        assert!(cancel.sleep(std::time::Duration::from_secs(30)));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(cancel.check("the guest").unwrap_err().contains("the guest"));
        Cancel::never().set();
    }

    #[test]
    fn a_transcript_answers_in_order_and_records_everything() {
        let mut ui = Transcript::answering(&["y", "n", "6.12"]);
        ui.say("one");
        assert!(ui.confirm("first?").unwrap());
        assert!(!ui.review("second?").unwrap());
        assert_eq!(ui.text("third?").unwrap(), "6.12");
        assert!(ui.confirm("fourth?").is_err());
        assert_eq!(ui.lines(), ["one"]);
        assert_eq!(ui.said[1], Said::Confirm("first?".to_owned(), true));
    }
}
