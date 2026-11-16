//! The settings form, for one VM or several, changing a managed VM's
//! settings or giving a VM being added its own, or setting the host up: a
//! labelled text box for each setting, checked as it is typed, with a word
//! about the chosen one, the command Save runs, and Cancel and Save. A
//! short window gets one-row boxes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Widget, Wrap};

use super::actions::Action;
use super::input::{History, Input};
use super::theme::{Icon, Theme};
use super::widgets::{self, raised_panel};
use crate::context::Context;
use crate::tone::Tone;

/// What a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A whole number.
    Number,
    /// A size with its unit.
    Size,
    /// A login, `USER@HOST`.
    Login,
    /// `true` or `false`.
    Bool,
    /// A libvirt connection URI.
    Uri,
}

/// The whole numbers a number setting takes.
fn range(key: &str) -> Option<(u64, u64)> {
    match key {
        "poll_interval_seconds" => Some((1, 10)),
        "command_timeout_seconds" | "convergence_timeout_seconds" => Some((1, 86_400)),
        "retention" => Some((2, 1000)),
        _ => None,
    }
}

/// What the form is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Changing managed VMs' settings: Save runs `set`.
    Set,
    /// Adding VMs: Save runs `vm add`, which shows what it will do and asks.
    Add,
    /// Setting the host up: Save runs `init`.
    Init,
    /// The host's global settings: Save runs `set global`.
    Global,
}

/// The host's memory and what its VMs hold, for the setup form to show the
/// pool its reserve leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostFacts {
    pub host_bytes: u64,
    /// What the host's VMs hold whatever happens, together.
    pub set_aside_bytes: u64,
    /// The VMs that hold memory now; shut-off ones that RAMpire doesn't
    /// manage aren't counted.
    pub vms: usize,
}

/// A setting's name in its file, its label, and what it means.
fn about(key: &str) -> (&'static str, &'static str, Kind) {
    match key {
        "priority" => (
            "Priority",
            "Higher is served first when memory is short. 100 is normal.",
            Kind::Number,
        ),
        "min_ram" => (
            "Minimum RAM",
            "The smallest the VM will be. It starts with this.",
            Kind::Size,
        ),
        "max_ram" => (
            "Maximum RAM",
            "The biggest it may grow. Linux: up to about 4 times its minimum.",
            Kind::Size,
        ),
        "free_ram" => (
            "Free RAM",
            "Spare memory kept in the VM. Below it the VM grows; above it, it shrinks.",
            Kind::Size,
        ),
        "guest_login" => (
            "Guest login",
            "SSH login for installing rampire-guest, as USER@HOST. Empty: the guest agent, or root at the VM's address.",
            Kind::Login,
        ),
        "host_reserve" => (
            "Host reserve",
            "Memory the host keeps for itself. VMs never get it.",
            Kind::Size,
        ),
        "pool" => (
            "Pool",
            "Memory the managed VMs share beyond their boot RAM. Needed with more than one VM.",
            Kind::Size,
        ),
        "observe_only" => (
            "Observe only",
            "true: decide and log everything, but change no VM's memory.",
            Kind::Bool,
        ),
        "poll_interval_seconds" => (
            "Poll interval",
            "Seconds between readings of the host and the guests, 1 to 10.",
            Kind::Number,
        ),
        "command_timeout_seconds" => (
            "Command limit",
            "Seconds each libvirt command may take.",
            Kind::Number,
        ),
        "convergence_timeout_seconds" => (
            "Settle limit",
            "Seconds a Windows removal may stall before it is settled. Linux uses 380.",
            Kind::Number,
        ),
        "libvirt_uri" => (
            "libvirt URI",
            "The libvirt connection, such as qemu:///system.",
            Kind::Uri,
        ),
        "retention" => (
            "Copies kept",
            "Applied copies kept for rolling back, 2 to 1000.",
            Kind::Number,
        ),
        _ => ("Setting", "", Kind::Size),
    }
}

/// One setting in the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Its name in the VM's file.
    pub key: &'static str,
    pub input: Input,
    /// Its value now, for one VM; for several, none: left empty, each
    /// keeps its own.
    pub original: Option<String>,
}

impl Field {
    pub fn new(key: &'static str, original: Option<String>) -> Self {
        let offers = match about(key).2 {
            Kind::Number if range(key).is_some() => Vec::new(),
            Kind::Number => vec!["100".to_owned(), "150".to_owned(), "50".to_owned()],
            Kind::Bool => vec!["false".to_owned(), "true".to_owned()],
            Kind::Uri => vec!["qemu:///system".to_owned()],
            Kind::Size => ["1GB", "2GB", "4GB", "8GB", "16GB", "512MB"]
                .iter()
                .map(|size| (*size).to_owned())
                .collect(),
            Kind::Login => Vec::new(),
        };
        let input = Input::new(original.as_deref().unwrap_or(""))
            .remembering(format!("setting {key}"))
            .offering(offers);
        Self {
            key,
            input,
            original,
        }
    }

    pub fn label(&self) -> &'static str {
        about(self.key).0
    }

    fn value(&self) -> &str {
        self.input.value().trim()
    }

    fn changed(&self) -> bool {
        match &self.original {
            Some(original) => self.value() != original,
            None => !self.value().is_empty(),
        }
    }

    /// The bytes it holds, when it is a size that reads.
    fn bytes(&self) -> Option<u64> {
        let value = self.value();
        if about(self.key).2 != Kind::Size || value.is_empty() {
            return None;
        }
        rampire_core::units::parse_byte_size(value).ok()
    }

    /// What is wrong with what is typed, by itself.
    fn problem(&self) -> Option<String> {
        let value = self.value();
        if value.is_empty() {
            return self.original.is_some().then(|| "needs a value".to_owned());
        }
        match about(self.key).2 {
            Kind::Number => match (value.parse::<u64>(), range(self.key)) {
                (Ok(number), Some((low, high))) if !(low..=high).contains(&number) => {
                    Some(format!("{low} to {high}"))
                }
                (Ok(number), None) if u32::try_from(number).is_err() => {
                    Some("a whole number, such as 100".to_owned())
                }
                (Ok(_), _) => None,
                (Err(_), Some((low, high))) => Some(format!("a whole number, {low} to {high}")),
                (Err(_), None) => Some("a whole number, such as 100".to_owned()),
            },
            Kind::Bool => (value != "true" && value != "false").then(|| "true or false".to_owned()),
            Kind::Uri => (!value.contains(":/") || value.contains(char::is_whitespace))
                .then(|| "a libvirt URI, such as qemu:///system".to_owned()),
            Kind::Size if value.chars().all(|character| character.is_ascii_digit()) => {
                Some(format!("add a unit: {value}GB or {value}MB"))
            }
            Kind::Size => rampire_core::units::parse_byte_size(value)
                .err()
                .map(|_| "a size, such as 8GB, 512MB or 1.5GB".to_owned()),
            Kind::Login => (!value.contains('@') || value.contains(char::is_whitespace))
                .then(|| "USER@HOST, such as root@192.168.122.5".to_owned()),
        }
    }
}

/// The settings form, for the VMs it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub purpose: Purpose,
    pub vms: Vec<String>,
    pub fields: Vec<Field>,
    pub cursor: usize,
    /// Said above the fields, in amber: what saving will do that a person
    /// may not expect, such as shutting a running VM down.
    pub warning: Option<String>,
    /// For the setup form: the host's memory, for the pool it leaves.
    pub host: Option<HostFacts>,
}

impl Form {
    fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.key == key)
    }

    /// What is wrong, by field: each field by itself, then the limits
    /// against each other.
    pub fn problems(&self) -> Vec<Option<String>> {
        let bytes = |key| self.field(key).and_then(Field::bytes);
        self.fields
            .iter()
            .map(|field| {
                field
                    .problem()
                    .or_else(|| match (field.key, bytes("max_ram")) {
                        ("max_ram", Some(max))
                            if bytes("min_ram").is_some_and(|min| min >= max) =>
                        {
                            Some("must be more than Minimum RAM".to_owned())
                        }
                        ("free_ram", Some(max))
                            if bytes("free_ram").is_some_and(|free| free >= max) =>
                        {
                            Some("must be less than Maximum RAM".to_owned())
                        }
                        ("host_reserve", _) => {
                            self.host.zip(field.bytes()).and_then(|(host, reserve)| {
                                crate::init::reserve_problem(host.host_bytes, reserve)
                            })
                        }
                        _ => None,
                    })
            })
            .collect()
    }

    pub fn valid(&self) -> bool {
        self.problems().iter().all(Option::is_none)
    }

    /// The changes, as `KEY=VALUE`.
    pub fn changes(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|field| field.changed())
            .map(|field| format!("{}={}", field.key, field.value()))
            .collect()
    }

    /// The commands Save runs.
    pub fn actions(&self) -> Vec<Action> {
        let changes = self.changes();
        match self.purpose {
            Purpose::Set if changes.is_empty() => Vec::new(),
            Purpose::Set => self
                .vms
                .iter()
                .map(|vm| Action::Set {
                    target: vm.clone(),
                    changes: changes.clone(),
                })
                .collect(),
            Purpose::Add => self
                .vms
                .iter()
                .map(|vm| Action::Add {
                    vm: vm.clone(),
                    changes: changes.clone(),
                })
                .collect(),
            Purpose::Global if changes.is_empty() => Vec::new(),
            Purpose::Global => vec![Action::Set {
                target: crate::set::GLOBAL.to_owned(),
                changes,
            }],
            Purpose::Init => vec![Action::Init {
                host_reserve: self
                    .field("host_reserve")
                    .map(|field| field.value().to_owned()),
            }],
        }
    }

    /// For the setup form: the host, its VMs and the pool the reserve
    /// leaves, as lines.
    fn host_lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let Some(host) = self.host else {
            return Vec::new();
        };
        let gib = |bytes: u64| bytes as f64 / rampire_core::GIB as f64;
        let mut lines = vec![widgets::line(
            theme,
            format!(
                "This host has {:.1} GiB. {} running VM{} hold {:.1} GiB; shut-off ones aren't counted.",
                gib(host.host_bytes),
                host.vms,
                if host.vms == 1 { "" } else { "s" },
                gib(host.set_aside_bytes)
            ),
            Tone::Dim,
        )];
        let reserve = self.field("host_reserve").and_then(Field::bytes);
        lines.push(match reserve {
            None => widgets::line(theme, "Pool: type a reserve to see it.", Tone::Dim),
            Some(reserve) => {
                match crate::init::pool_after(host.host_bytes, host.set_aside_bytes, reserve) {
                    Some(pool) => widgets::line(
                        theme,
                        format!("Pool for managed VMs: {:.0} GiB.", gib(pool)),
                        Tone::Good,
                    ),
                    None => widgets::line(
                        theme,
                        format!(
                            "{} Pool: none. The reserve and the VMs use all the memory.",
                            theme.icon(Icon::Problem)
                        ),
                        Tone::Warn,
                    ),
                }
            }
        });
        lines
    }

    pub fn render(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        history: &History,
        context: &Context,
    ) {
        let width = area.width.saturating_sub(4).min(84);
        let inner_width = width.saturating_sub(4);
        let actions = self.actions();
        let commands = actions.len().clamp(1, 3) as u16;
        let warning = self.warning.as_ref().map(|warning| {
            widgets::line(
                theme,
                format!("{} {warning}", theme.icon(Icon::Problem)),
                Tone::Warn,
            )
        });
        let host_lines = self.host_lines(theme);
        // Rows above the fields: the warning, wrapped, and a gap.
        let top = warning.as_ref().map_or(0, |line| {
            widgets::wrap(line.clone(), inner_width as usize).len() as u16 + 1
        });
        // Padding, the warning, the boxes, a gap, the host's lines, the
        // help, a rule, what Save runs, a gap, the buttons, and the border.
        let extra = top + host_lines.len() as u16 + u16::from(!host_lines.is_empty());
        let fixed = 1 + 1 + 2 + 1 + commands + 1 + 1 + 2 + extra;
        let rows = self.fields.len() as u16;
        let roomy = fixed + rows * 3 <= area.height.saturating_sub(2);
        let height = (fixed + rows * if roomy { 3 } else { 2 }).min(area.height.saturating_sub(2));
        let rect = widgets::centred(area, width, height);
        Clear.render(rect, buf);
        let (title, subtitle) = match (self.purpose, self.vms.as_slice()) {
            (Purpose::Set, [vm]) => (format!("Settings for {vm}"), "Enter: save and apply"),
            (Purpose::Set, vms) => (
                format!("Settings for {} VMs", vms.len()),
                "Enter: save and apply",
            ),
            (Purpose::Add, [vm]) => (format!("Add {vm}"), "Enter: review, then add"),
            (Purpose::Add, vms) => (format!("Add {} VMs", vms.len()), "Enter: review, then add"),
            (Purpose::Init, _) => ("Set up this host".to_owned(), "Enter: set up"),
            (Purpose::Global, _) => (
                "Global settings".to_owned(),
                "global.toml; Enter: save and apply",
            ),
        };
        let block =
            raised_panel(theme, Icon::Settings, &title, subtitle).padding(Padding::new(2, 2, 1, 0));
        let inner = block.inner(rect);
        block.render(rect, buf);
        let problems = self.problems();
        let mut y = inner.y;
        if let Some(warning) = warning {
            let rows = top.saturating_sub(1);
            Paragraph::new(warning)
                .wrap(Wrap { trim: true })
                .style(theme.raised())
                .render(
                    Rect {
                        height: rows,
                        ..inner
                    }
                    .intersection(inner),
                    buf,
                );
            y += top;
        }
        let label_width = self
            .fields
            .iter()
            .map(|field| field.label().len() as u16 + 2)
            .max()
            .unwrap_or(0)
            .max(14);
        let box_width = 24u16.min(inner_width.saturating_sub(label_width + 2));
        // When not every box fits, the ones shown start where the chosen
        // one is still in view.
        let step = if roomy { 3 } else { 2 };
        // Inside the border and the top padding, everything but the boxes
        // takes `fixed - 3` rows, the warning included.
        let room = inner.height.saturating_sub(fixed - 3) / step;
        let shown = usize::from(room.max(1));
        let first = self.cursor.saturating_sub(shown.saturating_sub(1));
        let hidden = (first, self.fields.len().saturating_sub(first + shown));
        for (index, field) in self.fields.iter().enumerate().skip(first).take(shown) {
            let chosen = index == self.cursor;
            let problem = problems.get(index).cloned().flatten();
            let row = Rect {
                y,
                height: if roomy { 3 } else { 1 },
                ..inner
            };
            if row.bottom() > inner.bottom() {
                break;
            }
            let middle = if roomy { y + 1 } else { y };
            // The label, beside the box's middle row.
            let label_style = if chosen { theme.accent() } else { theme.text() };
            buf.set_string(
                inner.x,
                middle,
                format!(
                    "{:<width$}",
                    format!("{}:", field.label()),
                    width = label_width as usize
                ),
                label_style,
            );
            // The box.
            let border = match (&problem, chosen) {
                (Some(_), _) => theme.tone(Tone::Bad),
                (None, true) => theme.accent(),
                (None, false) => theme.frame(),
            };
            let box_area = Rect {
                x: inner.x + label_width,
                y,
                width: box_width,
                height: row.height,
            };
            let text_area = if roomy {
                let frame = Block::new()
                    .borders(Borders::ALL)
                    .border_set(theme.border_set())
                    .border_style(border)
                    .style(theme.card())
                    .padding(Padding::horizontal(1));
                let text = frame.inner(box_area);
                frame.render(box_area, buf);
                text
            } else {
                // One row: the box as a field of its own colour, between
                // brackets.
                buf.set_string(box_area.x, y, "[", border);
                buf.set_string(box_area.right().saturating_sub(1), y, "]", border);
                let text = Rect {
                    x: box_area.x + 2,
                    width: box_width.saturating_sub(4),
                    ..box_area
                };
                buf.set_style(text, theme.card());
                text
            };
            let value = if chosen {
                field.input.line(theme, history, text_area.width as usize)
            } else if field.value().is_empty() && field.original.is_none() {
                let blank = match (self.purpose, field.key) {
                    (Purpose::Add, "guest_login") => "automatic",
                    (Purpose::Add | Purpose::Init, _) => "suggested",
                    (Purpose::Global, _) => "not set",
                    (Purpose::Set, _) => "each keeps its own",
                };
                Line::styled(blank, theme.dim())
            } else {
                Line::styled(field.value().to_owned(), theme.value())
            };
            Paragraph::new(value)
                .style(theme.card())
                .render(text_area, buf);
            // Beside the box: what is wrong, or what it was.
            let note = match (&problem, &field.original) {
                (Some(problem), _) => Some((
                    format!("{} {problem}", theme.icon(Icon::Problem)),
                    Tone::Bad,
                )),
                (None, Some(original)) if self.purpose == Purpose::Init => Some((
                    if field.changed() {
                        format!("recommended {original}")
                    } else {
                        "recommended".to_owned()
                    },
                    Tone::Dim,
                )),
                (None, Some(original)) if field.changed() => {
                    Some((format!("was {original}"), Tone::Dim))
                }
                _ => None,
            };
            if let Some((text, tone)) = note {
                let x = box_area.right() + 2;
                let room = inner.right().saturating_sub(x) as usize;
                buf.set_string(x, middle, widgets::fit(&text, room), theme.tone(tone));
            }
            y += if roomy { 3 } else { 2 };
        }
        // The host and the pool it leaves, for the setup form.
        if !host_lines.is_empty() {
            let rows = host_lines.len() as u16;
            Paragraph::new(host_lines).style(theme.raised()).render(
                Rect {
                    y: y + 1,
                    height: rows,
                    ..inner
                }
                .intersection(inner),
                buf,
            );
            y += rows + 1;
        }
        // A word about the chosen setting.
        let help_area = Rect {
            y: y + 1,
            height: 2,
            ..inner
        };
        if let Some(field) = self.fields.get(self.cursor) {
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}: ", field.label()), theme.value()),
                Span::styled(about(field.key).1, theme.text()),
            ]))
            .wrap(Wrap { trim: true })
            .style(theme.raised())
            .render(help_area.intersection(inner), buf);
        }
        // What Save runs, under a rule.
        let rule_y = help_area.bottom() + 1;
        if rule_y < inner.bottom() {
            let rule = if theme.icons.unicode { "─" } else { "-" };
            buf.set_string(
                inner.x,
                rule_y,
                rule.repeat(inner.width as usize),
                theme.frame(),
            );
        }
        let mut lines: Vec<Line> = if !self.valid() {
            vec![widgets::line(
                theme,
                format!(
                    "Fix the settings marked {} before saving.",
                    theme.icon(Icon::Problem)
                ),
                Tone::Warn,
            )]
        } else if actions.is_empty() {
            vec![widgets::line(theme, "Nothing changed yet.", Tone::Dim)]
        } else {
            let mut lines: Vec<Line> = actions
                .iter()
                .take(3)
                .enumerate()
                .map(|(index, action)| {
                    Line::from(vec![
                        Span::styled(if index == 0 { "Runs  " } else { "      " }, theme.dim()),
                        Span::styled(action.command_line(context), theme.tone(Tone::Info)),
                    ])
                })
                .collect();
            if actions.len() > 3 {
                lines.push(widgets::line(
                    theme,
                    format!("      and {} more", actions.len() - 3),
                    Tone::Dim,
                ));
            }
            lines
        };
        lines.truncate(commands as usize);
        let commands_area = Rect {
            y: rule_y + 1,
            height: commands,
            ..inner
        };
        Paragraph::new(lines)
            .style(theme.raised())
            .render(commands_area.intersection(inner), buf);
        // The buttons, at the bottom right.
        let buttons_y = inner.bottom().saturating_sub(1);
        let save_style = if self.valid() && !actions.is_empty() {
            theme.selected()
        } else {
            theme.dim()
        };
        let buttons = Line::from(vec![
            Span::styled(" Esc ", theme.key()),
            Span::styled(" Cancel ", theme.chip()),
            Span::styled("   ", theme.raised()),
            Span::styled(" Enter ", theme.key()),
            Span::styled(" Save ", save_style),
        ]);
        let width = buttons.width() as u16;
        let place = Rect {
            x: inner.right().saturating_sub(width),
            y: buttons_y,
            width: width.min(inner.width),
            height: 1,
        };
        Paragraph::new(buttons).render(place, buf);
        let hint = match hidden {
            (0, 0) => "Tab or arrows: the next setting".to_owned(),
            (above, below) => {
                format!("Tab: next setting; {above} above, {below} below")
            }
        };
        if inner.width as usize > hint.len() + width as usize + 2 {
            buf.set_string(inner.x, buttons_y, &hint, theme.dim());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{Depth, Icons, Palette};

    fn form() -> Form {
        Form {
            purpose: Purpose::Set,
            vms: vec!["web".to_owned()],
            fields: vec![
                Field::new("priority", Some("100".to_owned())),
                Field::new("min_ram", Some("2GB".to_owned())),
                Field::new("max_ram", Some("8GB".to_owned())),
                Field::new("free_ram", Some("1GB".to_owned())),
            ],
            cursor: 0,
            warning: None,
            host: None,
        }
    }

    #[test]
    fn the_form_changes_only_what_was_edited() {
        let mut form = form();
        form.fields[0].input.set("150");
        assert_eq!(form.changes(), ["priority=150"]);
        // For several VMs, a field left empty leaves each VM's own value.
        form.vms.push("db".to_owned());
        form.fields[2] = Field::new("max_ram", None);
        assert_eq!(form.actions().len(), 2);
        assert_eq!(form.changes(), ["priority=150"]);
        assert!(form.valid());
    }

    #[test]
    fn what_is_typed_is_checked_and_says_what_is_wrong() {
        let mut form = form();
        form.fields[0].input.set("high");
        form.fields[1].input.set("16GB");
        form.fields[3].input.set("8");
        let problems = form.problems();
        assert_eq!(problems[0].as_deref(), Some("a whole number, such as 100"));
        assert_eq!(problems[1], None);
        assert_eq!(
            problems[2].as_deref(),
            Some("must be more than Minimum RAM")
        );
        // Equal leaves nothing to grow into.
        form.fields[1].input.set("8GB");
        assert_eq!(
            form.problems()[2].as_deref(),
            Some("must be more than Minimum RAM")
        );
        form.fields[1].input.set("2GB");
        assert_eq!(form.problems()[2], None);
        assert_eq!(problems[3].as_deref(), Some("add a unit: 8GB or 8MB"));
        form.fields[3].input.set("8GB");
        assert_eq!(
            form.problems()[3].as_deref(),
            Some("must be less than Maximum RAM")
        );
        form.fields[3].input.set("");
        assert_eq!(form.problems()[3].as_deref(), Some("needs a value"));
        assert!(!form.valid());
    }

    #[test]
    fn the_form_draws_labelled_boxes_its_help_and_its_buttons() {
        let theme = Theme::new(Palette::Midnight, Depth::TrueColor, Icons { unicode: true });
        let context = Context {
            config: None,
            connect: None,
            build: String::new(),
            via: "tui",
        };
        let mut form = form();
        form.fields[0].input.set("150");
        for (width, height, roomy) in [(80, 22, false), (120, 38, true)] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            form.render(area, &mut buf, &theme, &History::default(), &context);
            let text = widgets::plain_text(&buf);
            for needle in [
                "Settings for web",
                "Priority:",
                "Minimum RAM:",
                "Free RAM:",
                "was 100",
                "Higher is served first",
                "Runs  rampire set web priority=150",
                "Cancel",
                "Save",
            ] {
                assert!(
                    text.contains(needle),
                    "{width}x{height} lacks {needle}:\n{text}"
                );
            }
            assert_eq!(text.contains("╭────"), roomy, "{text}");
            assert!(!text.contains("free_ram"), "{text}");
        }
    }

    #[test]
    fn the_setup_form_shows_the_pool_its_reserve_leaves_and_a_warning() {
        use rampire_core::GIB;
        let theme = Theme::new(Palette::Midnight, Depth::TrueColor, Icons { unicode: true });
        let context = Context {
            config: None,
            connect: None,
            build: String::new(),
            via: "tui",
        };
        let mut form = Form {
            purpose: Purpose::Init,
            vms: Vec::new(),
            fields: vec![Field::new("host_reserve", Some("4GB".to_owned()))],
            cursor: 0,
            warning: None,
            host: Some(HostFacts {
                host_bytes: 32 * GIB,
                set_aside_bytes: 20 * GIB,
                vms: 3,
            }),
        };
        let draw = |form: &Form| {
            let area = Rect::new(0, 0, 100, 30);
            let mut buf = Buffer::empty(area);
            form.render(area, &mut buf, &theme, &History::default(), &context);
            widgets::plain_text(&buf)
        };
        let text = draw(&form);
        for needle in [
            "Set up this host",
            "Host reserve:",
            "recommended",
            "Pool for managed VMs: 8 GiB.",
            "Runs  rampire init --host-reserve 4GB",
        ] {
            assert!(text.contains(needle), "lacks {needle}:\n{text}");
        }
        // Edited, it is still checked, and the pool follows it.
        form.fields[0].input.set("13GB");
        assert!(draw(&form).contains("Pool: none."), "{}", draw(&form));
        form.fields[0].input.set("40GB");
        assert!(!form.valid());
        form.fields[0].input.set("6GB");
        assert_eq!(
            form.actions(),
            [Action::Init {
                host_reserve: Some("6GB".to_owned())
            }]
        );
        // An add form for a running VM says it will be shut down.
        let add = Form {
            purpose: Purpose::Add,
            vms: vec!["web".to_owned()],
            fields: vec![Field::new("priority", Some("100".to_owned()))],
            cursor: 0,
            warning: Some(
                "web is running. Adding it shuts it down and starts it again.".to_owned(),
            ),
            host: None,
        };
        let text = draw(&add);
        assert!(
            text.contains("web is running. Adding it shuts it"),
            "{text}"
        );
    }

    #[test]
    fn the_global_form_checks_each_setting_and_runs_set_global() {
        let theme = Theme::new(Palette::Midnight, Depth::TrueColor, Icons { unicode: true });
        let context = Context {
            config: None,
            connect: None,
            build: String::new(),
            via: "tui",
        };
        let field = |key, value: Option<&str>| Field::new(key, value.map(str::to_owned));
        let mut form = Form {
            purpose: Purpose::Global,
            vms: Vec::new(),
            fields: vec![
                field("pool", None),
                field("host_reserve", Some("2GB")),
                field("observe_only", Some("false")),
                field("poll_interval_seconds", Some("5")),
                field("command_timeout_seconds", Some("10")),
                field("convergence_timeout_seconds", Some("300")),
                field("libvirt_uri", Some("qemu:///system")),
                field("retention", Some("10")),
            ],
            cursor: 0,
            warning: None,
            host: None,
        };
        assert!(form.valid());
        assert!(form.actions().is_empty());
        form.fields[0].input.set("8GB");
        form.fields[2].input.set("yes");
        form.fields[3].input.set("11");
        form.fields[6].input.set("qemu system");
        form.fields[7].input.set("1");
        let problems = form.problems();
        assert_eq!(problems[0], None, "an unset pool may be set");
        assert_eq!(problems[2].as_deref(), Some("true or false"));
        assert_eq!(problems[3].as_deref(), Some("1 to 10"));
        assert_eq!(
            problems[6].as_deref(),
            Some("a libvirt URI, such as qemu:///system")
        );
        assert_eq!(problems[7].as_deref(), Some("2 to 1000"));
        for (index, value) in [(2, "true"), (3, "3"), (6, "qemu:///system"), (7, "10")] {
            form.fields[index].input.set(value);
        }
        assert_eq!(
            form.actions(),
            [Action::Set {
                target: crate::set::GLOBAL.to_owned(),
                changes: vec![
                    "pool=8GB".to_owned(),
                    "observe_only=true".to_owned(),
                    "poll_interval_seconds=3".to_owned()
                ],
            }]
        );
        // In a short window the boxes scroll so the chosen one shows.
        form.cursor = 7;
        let area = Rect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);
        form.render(area, &mut buf, &theme, &History::default(), &context);
        let text = widgets::plain_text(&buf);
        assert!(text.contains("Global settings"), "{text}");
        assert!(text.contains("Copies kept:"), "{text}");
        assert!(text.contains("above, 0 below"), "{text}");
        assert!(
            text.contains("Runs  rampire set global pool=8GB observe_only=true"),
            "{text}"
        );
    }
}
