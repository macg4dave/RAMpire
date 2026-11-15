//! The guided flows the commands and the TUI share: each takes a
//! [`Context`](crate::context::Context) and talks to the person through a
//! [`Ui`](crate::ui::Ui), so the commands answer on the terminal and the TUI
//! in its dialogs.

pub mod apply;
pub mod controller;
pub mod lifecycle;

/// How a flow that didn't fail ended. Returning normally doesn't mean it
/// did its job: the person may have answered No, or the job may have been
/// done already, or done only in part. Callers report from this, never
/// from `Ok` alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It did what it was started for.
    Done,
    /// Nothing needed doing; the text says why.
    AlreadySo(String),
    /// The person answered No. Nothing was changed after the question;
    /// the text says what, if anything, was changed before it.
    Declined(String),
    /// Part of it is done; the text says what is left and how to finish.
    Partly(String),
}

impl Outcome {
    /// It did what it was started for, or that was so already.
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Done | Self::AlreadySo(_))
    }

    /// A declined question that changed nothing.
    pub fn declined() -> Self {
        Self::Declined("nothing was changed".to_owned())
    }
}
