//! The guided flows the commands and the TUI share: each takes a
//! [`Context`](crate::context::Context) and talks to the person through a
//! [`Ui`](crate::ui::Ui), so the commands answer on the terminal and the TUI
//! in its dialogs.

pub mod apply;
pub mod controller;
pub mod lifecycle;
