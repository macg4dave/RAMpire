//! The settings form, for one VM or several, changing a managed VM's
//! settings or giving a VM being added its own: a labelled text box for each
//! setting, checked as it is typed, with a word about the chosen one, the
//! command Save runs, and Cancel and Save. A short window gets one-row
//! boxes.

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
}

/// What the form is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Changing managed VMs' settings: Save runs `set`.
    Set,
    /// Adding VMs: Save runs `vm add`, which shows what it will do and asks.
    Add,
}

/// A setting's name in its file, its label, and what it means.
fn about(key: &str) -> (&'static str, &'static str, Kind) {
    match key {
        "priority" => (
            "Priority",
            "Who is served first when memory is short: a higher number goes first. 100 is usual.",
            Kind::Number,
        ),
        "min_ram" => (
            "Minimum RAM",
            "The least memory the VM keeps, its boot memory included. RAMpire never takes memory back below this.",
            Kind::Size,
        ),
        "max_ram" => (
            "Maximum RAM",
            "The most memory the VM may grow to. For Linux, at most about four times its boot memory.",
            Kind::Size,
        ),
        "free_ram" => (
            "Free RAM",
            "Spare memory the VM keeps ready: it asks for more when it has less than this, and gives back what is over.",
            Kind::Size,
        ),
        "guest_login" => (
            "Guest login",
            "How RAMpire logs in to the VM to install rampire-guest, as USER@HOST. Left empty: its guest agent, or root at the address libvirt has for it, with RAMpire's SSH key.",
            Kind::Login,
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
            Kind::Number => vec!["100".to_owned(), "150".to_owned(), "50".to_owned()],
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
            Kind::Number => value
                .parse::<u32>()
                .err()
                .map(|_| "a whole number, such as 100".to_owned()),
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
                        ("min_ram", Some(max)) if bytes("min_ram").is_some_and(|min| min > max) => {
                            Some("more than Maximum RAM".to_owned())
                        }
                        ("free_ram", Some(max))
                            if bytes("free_ram").is_some_and(|free| free >= max) =>
                        {
                            Some("must be less than Maximum RAM".to_owned())
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
        }
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
        // Padding, the boxes, a gap, the help, a rule, what Save runs, a
        // gap, the buttons, and the border.
        let fixed = 1 + 1 + 2 + 1 + commands + 1 + 1 + 2;
        let rows = self.fields.len() as u16;
        let roomy = fixed + rows * 3 <= area.height.saturating_sub(2);
        let height = (fixed + rows * if roomy { 3 } else { 2 }).min(area.height.saturating_sub(2));
        let rect = widgets::centred(area, width, height);
        Clear.render(rect, buf);
        let (title, subtitle) = match (self.purpose, self.vms.as_slice()) {
            (Purpose::Set, [vm]) => (format!("Settings for {vm}"), "saved and applied with Enter"),
            (Purpose::Set, vms) => (
                format!("Settings for {} VMs", vms.len()),
                "saved and applied with Enter",
            ),
            (Purpose::Add, [vm]) => (format!("Add {vm}"), "Enter shows what it will do"),
            (Purpose::Add, vms) => (
                format!("Add {} VMs", vms.len()),
                "Enter shows what it will do",
            ),
        };
        let block =
            raised_panel(theme, Icon::Settings, &title, subtitle).padding(Padding::new(2, 2, 1, 0));
        let inner = block.inner(rect);
        block.render(rect, buf);
        let problems = self.problems();
        let mut y = inner.y;
        let label_width = 14u16;
        let box_width = 24u16.min(inner_width.saturating_sub(label_width + 2));
        for (index, field) in self.fields.iter().enumerate() {
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
                    (Purpose::Add, _) => "suggested",
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
                        Span::styled(
                            if index == 0 {
                                "Saving runs  "
                            } else {
                                "             "
                            },
                            theme.dim(),
                        ),
                        Span::styled(action.command_line(context), theme.tone(Tone::Info)),
                    ])
                })
                .collect();
            if actions.len() > 3 {
                lines.push(widgets::line(
                    theme,
                    format!("             and {} more", actions.len() - 3),
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
        let hint = "Tab or arrows: the next setting";
        if inner.width as usize > hint.len() + width as usize + 2 {
            buf.set_string(inner.x, buttons_y, hint, theme.dim());
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
        assert_eq!(problems[1].as_deref(), Some("more than Maximum RAM"));
        assert_eq!(problems[2], None);
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
                "Who is served first",
                "Saving runs  rampire set web priority=150",
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
}
