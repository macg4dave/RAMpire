//! A VM's facts at a glance, as Yazi's spotter shows a file's: Tab opens a
//! table of what is known about the chosen VM, Left and Right go through
//! the VMs without closing it, and `y` copies the chosen fact. The preview
//! beside the list shows the same for a VM RAMpire doesn't manage.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use super::app::{App, Entry};
use super::scroll::Scroll;
use super::theme::{Icon, Theme};
use super::widgets::{self, panel, raised_panel};
use crate::reports::{Management, Published};
use crate::tone::Tone;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot {
    pub vm: String,
    pub scroll: Scroll,
}

/// One fact: what it is, and its value.
pub type Fact = (String, String);

/// What is known about `entry`, from `vm list`, the pool's readings and its
/// settings, with the commands that show more.
pub fn facts(app: &App, entry: &Entry) -> Vec<Fact> {
    let vm = entry.name.as_str();
    let mut facts: Vec<Fact> = Vec::new();
    let mut fact = |label: &str, value: String| facts.push((label.to_owned(), value));
    fact("Name", vm.to_owned());
    fact("RAMpire", entry.management.label().to_owned());
    if let Some(listed) = &entry.listed {
        fact("State", listed.state.clone());
        fact("Status", listed.detail.clone());
        fact(
            "Guest service",
            match listed.guest {
                Published::Yes { age_seconds } => format!("running, last heard {age_seconds}s ago"),
                Published::Installed => "installed".to_owned(),
                Published::No => "not installed yet: adding the VM installs it".to_owned(),
                Published::Unknown => "unknown".to_owned(),
            },
        );
        if let Some(os) = &listed.os_id {
            fact("System", os.clone());
        }
        for change in &listed.to_fix {
            fact("Adding it will", change.clone());
        }
        for problem in &listed.problems {
            fact("Problem", problem.problem.clone());
            fact("To fix it", problem.fix.clone());
        }
    }
    if let (Some(index), Some(Ok(pool))) = (entry.pool_index, &app.pool) {
        if let (Some(settings), Some(sample)) = (pool.command.vms.get(index), pool.vms.get(index)) {
            fact("Priority", settings.priority.to_string());
            fact(
                "Minimum RAM",
                rampire_core::format_byte_size(settings.min_ram),
            );
            fact(
                "Maximum RAM",
                rampire_core::format_byte_size(settings.max_ram),
            );
            match sample.device {
                Some(device) => {
                    fact("Device size", crate::reports::size(device.size_bytes));
                    fact("Requested", crate::reports::size(device.requested_bytes));
                    fact("Plugged in", crate::reports::size(device.current_bytes));
                    fact("Block size", crate::reports::size(device.block_size_bytes));
                }
                None => fact(
                    "Device",
                    "can't be read: the VM is stopped, or libvirt didn't answer".to_owned(),
                ),
            }
            if let Some(guest) = &sample.guest {
                let memory = &guest.memory;
                fact(
                    "Guest memory",
                    crate::reports::size(memory.physical_total_bytes),
                );
                fact(
                    "Guest available",
                    crate::reports::size(memory.physical_available_bytes),
                );
                fact(
                    "Guest request",
                    guest.request.as_ref().map_or("none".to_owned(), |request| {
                        format!(
                            "{:?} {}",
                            request.operation,
                            crate::reports::size(request.bytes)
                        )
                    }),
                );
            }
        }
    }
    if let Some(details) = app.details.get(vm) {
        if let Some(boot) = details.boot_bytes {
            fact("Boot RAM", crate::reports::size(boot));
        }
    }
    let line = |words: &[&str]| app.context.command_line(words);
    if entry.management == Management::Managed {
        fact("Watch it live", line(&["watch", vm]));
        fact("Its logs", line(&["logs", "--vm", vm]));
    }
    fact("Check it", line(&["doctor", vm]));
    facts
}

/// `facts` as a two-column table in `area`, the chosen row marked.
pub fn render_facts(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    facts: &[Fact],
    scroll: Option<&Scroll>,
    style: ratatui::style::Style,
) {
    let height = area.height as usize;
    let range = match scroll {
        Some(scroll) => scroll.window(facts.len(), height),
        None => 0..facts.len().min(height),
    };
    let cursor = scroll.map(|scroll| scroll.cursor(facts.len()));
    let value_width = area.width.saturating_sub(18) as usize;
    let lines: Vec<Line> = range
        .filter_map(|index| facts.get(index).map(|fact| (index, fact)))
        .map(|(index, (label, value))| {
            let value_style = if Some(index) == cursor {
                theme.selected()
            } else if label == "Problem" {
                theme.tone(Tone::Bad)
            } else {
                theme.value()
            };
            Line::from(vec![
                Span::styled(format!("{label:<17} "), theme.dim()),
                Span::styled(widgets::fit(value, value_width), value_style),
            ])
        })
        .collect();
    Paragraph::new(lines).style(style).render(area, buf);
}

impl Spot {
    pub fn render(&self, area: Rect, buf: &mut Buffer, app: &App) {
        let theme = &app.theme;
        let Some(entry) = app.entry(&self.vm) else {
            return;
        };
        let facts = facts(app, &entry);
        let rect = widgets::centred(
            area,
            area.width.saturating_sub(8).min(96),
            (facts.len() as u16 + 2).min(area.height.saturating_sub(2)),
        );
        Clear.render(rect, buf);
        let block = raised_panel(
            theme,
            Icon::Vm,
            &format!("{} facts", self.vm),
            "y copies one",
        );
        let inner = block.inner(rect);
        block.render(rect, buf);
        render_facts(
            inner,
            buf,
            theme,
            &facts,
            Some(&self.scroll),
            theme.raised(),
        );
    }
}

/// The preview of a VM RAMpire doesn't run: its facts, in a panel.
pub fn render_preview(area: Rect, buf: &mut Buffer, app: &App, entry: &Entry) {
    let theme = &app.theme;
    let block = panel(theme, Icon::Vm, &entry.name, entry.management.label());
    let inner = block.inner(area);
    block.render(area, buf);
    render_facts(inner, buf, theme, &facts(app, entry), None, theme.card());
}
