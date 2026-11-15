//! Colour themes and icon sets. A screen names what something means (a
//! [`Tone`], an [`Icon`]); the theme gives it a colour and the icon set a
//! symbol, so nothing is shown by colour alone and every screen works in
//! true colour, 256 colours, 16 colours, no colour, Unicode and ASCII.
//!
//! The default, midnight, takes its colours from the mascot: near-black
//! purple, glowing violet and magenta, white titles, the RAM stick's green,
//! the blood drop's crimson.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;

use crate::tone::Tone;

/// The colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// 24-bit colour.
    TrueColor,
    Colours256,
    Colours16,
    /// `NO_COLOR` or `TERM=dumb`: emphasis only.
    None,
}

impl Depth {
    /// From the environment the TUI runs in. `NO_COLOR` or `TERM=dumb` is
    /// none; a `256color` or `direct` terminal, or `COLORTERM`, is true
    /// colour, since today's terminals all have it and SSH and sudo pass
    /// on only `TERM` (the 256-colour midnight theme is a `t` away); any
    /// other is 16.
    pub fn detect(term: Option<&str>, colorterm: Option<&str>, no_color: bool) -> Self {
        let term = term.unwrap_or("");
        if no_color || term == "dumb" || term.is_empty() {
            Self::None
        } else if term.contains("256color")
            || term.contains("direct")
            || colorterm.is_some_and(|value| !value.is_empty())
        {
            Self::TrueColor
        } else {
            Self::Colours16
        }
    }

    pub fn from_environment() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self::detect(
            var("TERM").as_deref(),
            var("COLORTERM").as_deref(),
            std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
        )
    }

    /// The depth after asking the terminal: one that kept a 24-bit colour
    /// has true colour whatever its `TERM`, and one that gave back one of
    /// 256 has 256, however `TERM` reads. No colour stays no colour.
    pub fn probed(self, true_colour: Option<bool>) -> Self {
        match (self, true_colour) {
            (Self::None, _) | (_, None) => self,
            (_, Some(true)) => Self::TrueColor,
            (Self::TrueColor | Self::Colours256, Some(false)) => Self::Colours256,
            (Self::Colours16, Some(false)) => Self::Colours16,
        }
    }
}

/// The palettes a person can choose between with `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// The mascot's colours on near-black.
    Midnight,
    /// Midnight in the 256 colours every terminal has, for one without
    /// true colour.
    Midnight256,
    /// Dark text on a light background.
    Light,
    /// The terminal's own 16 colours on its own background,
    /// so it matches a person's terminal theme.
    Terminal,
}

impl Palette {
    pub const ALL: [Self; 4] = [
        Self::Midnight,
        Self::Midnight256,
        Self::Light,
        Self::Terminal,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Midnight => "midnight",
            Self::Midnight256 => "midnight, 256 colours",
            Self::Light => "light",
            Self::Terminal => "terminal",
        }
    }

    /// The palettes in the order `t` goes through them: on a light
    /// terminal, light comes before midnight.
    pub fn order(light: bool) -> [Self; 4] {
        if light {
            [
                Self::Midnight,
                Self::Light,
                Self::Midnight256,
                Self::Terminal,
            ]
        } else {
            Self::ALL
        }
    }

    pub fn next(self, light: bool) -> Self {
        let order = Self::order(light);
        let at = order
            .iter()
            .position(|palette| *palette == self)
            .unwrap_or(0);
        order[(at + 1) % order.len()]
    }
}

/// A colour as red, green and blue.
type Rgb = (u8, u8, u8);

/// Every colour role a screen uses.
#[derive(Debug, Clone, Copy)]
struct Colours {
    /// Behind everything.
    background: Rgb,
    /// Inside a panel: a little lighter, so panels read as cards.
    card: Rgb,
    /// A dialog, lighter again.
    raised: Rgb,
    text: Rgb,
    value: Rgb,
    dim: Rgb,
    frame: Rgb,
    /// The pill behind a panel's title.
    chip: Rgb,
    chip_text: Rgb,
    /// The header and status bars.
    bar: Rgb,
    bar_text: Rgb,
    /// A key in the status bar's hints.
    key: Rgb,
    track: Rgb,
    good: Rgb,
    warn: Rgb,
    bad: Rgb,
    info: Rgb,
    accent: Rgb,
    selected: Rgb,
    series: [Rgb; 8],
}

const MIDNIGHT: Colours = Colours {
    background: (0x05, 0x03, 0x07),
    card: (0x0b, 0x08, 0x10),
    raised: (0x16, 0x10, 0x1e),
    text: (0xec, 0xe8, 0xf2),
    value: (0xff, 0xff, 0xff),
    dim: (0xb4, 0xaa, 0xc2),
    frame: (0x9b, 0x45, 0xd6),
    chip: (0xb8, 0x3b, 0xe0),
    chip_text: (0xff, 0xff, 0xff),
    bar: (0x12, 0x0c, 0x19),
    bar_text: (0xec, 0xe8, 0xf2),
    key: (0x5a, 0x24, 0x80),
    track: (0x2c, 0x26, 0x34),
    good: (0x9c, 0xe0, 0x5a),
    warn: (0xf5, 0xc8, 0x42),
    bad: (0xf0, 0x3e, 0x6a),
    info: (0xd4, 0x7d, 0xff),
    accent: (0xe0, 0x4c, 0xe8),
    selected: (0x2c, 0x18, 0x44),
    series: [
        (0xe0, 0x4c, 0xe8),
        (0x9c, 0xe0, 0x5a),
        (0xb0, 0x8c, 0xff),
        (0xff, 0x8a, 0xc8),
        (0x6e, 0xd8, 0xc8),
        (0xf5, 0xc8, 0x42),
        (0x8a, 0x6c, 0xf0),
        (0xff, 0xa8, 0x6a),
    ],
};

const LIGHT: Colours = Colours {
    background: (0xf6, 0xf1, 0xfb),
    card: (0xff, 0xff, 0xff),
    raised: (0xff, 0xff, 0xff),
    text: (0x3a, 0x2a, 0x4e),
    value: (0x1a, 0x0b, 0x2b),
    dim: (0x8a, 0x7a, 0x9e),
    frame: (0xa4, 0x5c, 0xd6),
    chip: (0x8e, 0x2b, 0xc2),
    chip_text: (0xff, 0xff, 0xff),
    bar: (0x5b, 0x21, 0x86),
    bar_text: (0xff, 0xff, 0xff),
    key: (0x8e, 0x2b, 0xc2),
    track: (0xe6, 0xdb, 0xf0),
    good: (0x3a, 0x8a, 0x1e),
    warn: (0xa8, 0x6a, 0x00),
    bad: (0xc8, 0x1e, 0x4a),
    info: (0x7a, 0x2b, 0xc2),
    accent: (0xb0, 0x1e, 0xb8),
    selected: (0xea, 0xd6, 0xfa),
    series: [
        (0xb0, 0x1e, 0xb8),
        (0x3a, 0x8a, 0x1e),
        (0x5a, 0x3c, 0xc8),
        (0xc8, 0x3a, 0x7a),
        (0x1e, 0x8a, 0x7a),
        (0xa8, 0x6a, 0x00),
        (0x6a, 0x3c, 0x9e),
        (0xc8, 0x5a, 0x1e),
    ],
};

/// The nearest of the 256 colours to `rgb`: from the 6x6x6 cube or the
/// grey ramp, whichever is closer.
pub fn nearest_256((red, green, blue): Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |value: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| (i32::from(**level) - i32::from(value)).abs())
            .map_or(0, |(index, _)| index as u8)
    };
    let distance = |(r, g, b): Rgb| {
        let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
        d(r, red) + d(g, green) + d(b, blue)
    };
    let (r, g, b) = (level(red), level(green), level(blue));
    let cube = 16 + 36 * r + 6 * g + b;
    let cube_rgb = (LEVELS[r as usize], LEVELS[g as usize], LEVELS[b as usize]);
    let average = (u16::from(red) + u16::from(green) + u16::from(blue)) / 3;
    let grey_step = (average.saturating_sub(8) / 10).min(23) as u8;
    let grey_value = 8 + 10 * grey_step;
    let grey = 232 + grey_step;
    if distance((grey_value, grey_value, grey_value)) < distance(cube_rgb) {
        grey
    } else {
        cube
    }
}

/// The look in force: a palette at the terminal's depth, and an icon set.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub palette: Palette,
    pub depth: Depth,
    pub icons: Icons,
    /// The terminal's background is light, as it said when asked.
    pub light: bool,
}

/// The 16 named colours' roles, for the terminal palette and 16-colour
/// terminals.
#[derive(Debug, Clone, Copy)]
enum Named {
    Text,
    Dim,
    Frame,
    Good,
    Warn,
    Bad,
    Info,
    Accent,
}

impl Theme {
    pub fn new(palette: Palette, depth: Depth, icons: Icons) -> Self {
        Self {
            palette,
            depth,
            icons,
            light: false,
        }
    }

    /// On a terminal whose background is light.
    pub fn on_light(mut self, light: bool) -> Self {
        self.light = light;
        self
    }

    /// The next palette for `t`.
    pub fn next_palette(&mut self) {
        self.palette = self.palette.next(self.light);
    }

    /// The palette's colours, when it has its own (not the terminal's).
    fn colours(&self) -> Option<&'static Colours> {
        match (self.depth, self.palette) {
            (Depth::None | Depth::Colours16, _) | (_, Palette::Terminal) => None,
            (_, Palette::Midnight | Palette::Midnight256) => Some(&MIDNIGHT),
            (_, Palette::Light) => Some(&LIGHT),
        }
    }

    fn colour(&self, rgb: Rgb) -> Color {
        if self.depth == Depth::TrueColor && self.palette != Palette::Midnight256 {
            Color::Rgb(rgb.0, rgb.1, rgb.2)
        } else {
            Color::Indexed(nearest_256(rgb))
        }
    }

    fn named(&self, named: Named) -> Option<Color> {
        if self.depth == Depth::None {
            return None;
        }
        Some(match named {
            Named::Text => Color::Reset,
            // Light grey disappears on a light background.
            Named::Dim if self.light => Color::DarkGray,
            Named::Dim => Color::Gray,
            Named::Frame | Named::Info => Color::Magenta,
            Named::Good => Color::Green,
            Named::Warn => Color::Yellow,
            Named::Bad => Color::Red,
            Named::Accent => Color::LightMagenta,
        })
    }

    /// A foreground colour only, so the panel's own background shows.
    fn fg(&self, pick: impl Fn(&Colours) -> Rgb, named: Named) -> Style {
        match self.colours() {
            Some(colours) => Style::new().fg(self.colour(pick(colours))),
            None => self
                .named(named)
                .map_or(Style::new(), |colour| Style::new().fg(colour)),
        }
    }

    fn bg(&self, pick: impl Fn(&Colours) -> Rgb) -> Style {
        match self.colours() {
            Some(colours) => Style::new().bg(self.colour(pick(colours))),
            None => Style::new(),
        }
    }

    /// Behind everything.
    pub fn base(&self) -> Style {
        self.bg(|colours| colours.background).patch(self.text())
    }

    /// A panel's inside: a card a little lighter than the background.
    pub fn card(&self) -> Style {
        self.bg(|colours| colours.card).patch(self.text())
    }

    /// A dialog: raised above the cards.
    pub fn raised(&self) -> Style {
        self.bg(|colours| colours.raised).patch(self.text())
    }

    pub fn text(&self) -> Style {
        self.fg(|colours| colours.text, Named::Text)
    }

    /// A figure: white and bold.
    pub fn value(&self) -> Style {
        self.fg(|colours| colours.value, Named::Text)
            .add_modifier(Modifier::BOLD)
    }

    /// Secondary text: dimmer, and in italics where the terminal has them.
    pub fn dim(&self) -> Style {
        let style = self.fg(|colours| colours.dim, Named::Dim);
        if self.depth == Depth::None {
            style
        } else {
            style.add_modifier(Modifier::ITALIC)
        }
    }

    pub fn frame(&self) -> Style {
        self.fg(|colours| colours.frame, Named::Frame)
    }

    pub fn title(&self) -> Style {
        self.value()
    }

    /// The colour of a kind of panel, named by its icon, so panels stand
    /// apart: the pool green, VMs magenta, a device blue, a guest cyan,
    /// settings gold, problems red.
    pub fn hue(&self, icon: Icon) -> Option<Color> {
        let (rgb, named) = match icon {
            Icon::Pool | Icon::Ok => ((0x9c, 0xe0, 0x5a), Color::LightGreen),
            Icon::Vm | Icon::Pointer => ((0xe0, 0x4c, 0xe8), Color::LightMagenta),
            Icon::Device | Icon::Host => ((0x7c, 0x9c, 0xff), Color::LightBlue),
            Icon::Guest => ((0x5c, 0xd8, 0xe0), Color::LightCyan),
            Icon::Settings => ((0xf5, 0xc8, 0x42), Color::LightYellow),
            Icon::Problem | Icon::Latched => ((0xf0, 0x3e, 0x6a), Color::LightRed),
            _ => ((0xb0, 0x8c, 0xff), Color::LightMagenta),
        };
        match (self.depth, self.colours()) {
            (Depth::None, _) => None,
            (_, Some(_)) => Some(self.colour(rgb)),
            (_, None) => Some(named),
        }
    }

    /// A panel's border, in its kind's colour.
    pub fn frame_of(&self, icon: Icon) -> Style {
        self.hue(icon)
            .map_or_else(|| self.frame(), |hue| Style::new().fg(hue))
    }

    /// A panel's title, as a block of its kind's colour.
    pub fn chip_of(&self, icon: Icon) -> Style {
        match self.hue(icon) {
            Some(hue) => Style::new()
                .fg(Color::Black)
                .bg(hue)
                .add_modifier(Modifier::BOLD),
            None => self.chip(),
        }
    }

    /// A panel's title, as a pill: white on magenta.
    pub fn chip(&self) -> Style {
        match self.colours() {
            Some(colours) => Style::new()
                .fg(self.colour(colours.chip_text))
                .bg(self.colour(colours.chip))
                .add_modifier(Modifier::BOLD),
            None if self.depth == Depth::None => {
                Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
            }
            None => Style::new()
                .fg(Color::White)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        }
    }

    /// The header and status bars.
    pub fn bar(&self) -> Style {
        match self.colours() {
            Some(colours) => Style::new()
                .fg(self.colour(colours.bar_text))
                .bg(self.colour(colours.bar)),
            None if self.depth == Depth::None => Style::new().add_modifier(Modifier::REVERSED),
            None => Style::new().fg(Color::White).bg(Color::Magenta),
        }
    }

    /// A key in the status bar, as a keycap.
    pub fn key(&self) -> Style {
        match self.colours() {
            Some(colours) => Style::new()
                .fg(self.colour(colours.value))
                .bg(self.colour(colours.key))
                .add_modifier(Modifier::BOLD),
            None if self.depth == Depth::None => Style::new().add_modifier(Modifier::BOLD),
            None => Style::new()
                .fg(Color::Black)
                .bg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
        }
    }

    /// The accent: the magenta of the mascot's cape.
    pub fn accent(&self) -> Style {
        self.fg(|colours| colours.accent, Named::Accent)
            .add_modifier(Modifier::BOLD)
    }

    /// The empty part of a gauge.
    pub fn track(&self) -> Style {
        self.fg(|colours| colours.track, Named::Dim)
    }

    /// A selected row or item.
    pub fn selected(&self) -> Style {
        match self.colours() {
            Some(colours) => Style::new()
                .fg(self.colour(colours.value))
                .bg(self.colour(colours.selected))
                .add_modifier(Modifier::BOLD),
            None => Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        }
    }

    /// A table's chosen row: the selection's background, with each cell
    /// keeping its own colours, so a bar still shows how full it is.
    pub fn selected_row(&self) -> Style {
        match self.colours() {
            Some(colours) => Style::new()
                .bg(self.colour(colours.selected))
                .add_modifier(Modifier::BOLD),
            None => Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        }
    }

    pub fn tone(&self, tone: Tone) -> Style {
        match tone {
            Tone::Normal => self.text(),
            Tone::Value => self.value(),
            Tone::Dim => self.dim(),
            Tone::Good => self.fg(|colours| colours.good, Named::Good),
            Tone::Warn => self
                .fg(|colours| colours.warn, Named::Warn)
                .add_modifier(Modifier::BOLD),
            Tone::Bad => self
                .fg(|colours| colours.bad, Named::Bad)
                .add_modifier(Modifier::BOLD),
            Tone::Info => self.fg(|colours| colours.info, Named::Info),
            Tone::Accent => self.accent(),
        }
    }

    /// A colour for each VM in the pool's bar and history, in host-list
    /// order. Without colour every VM is the same, and the legend names
    /// them.
    pub fn series(&self, index: usize) -> Style {
        const NAMED: [Color; 6] = [
            Color::Magenta,
            Color::Green,
            Color::Blue,
            Color::LightMagenta,
            Color::Cyan,
            Color::Yellow,
        ];
        match (self.colours(), self.depth) {
            (_, Depth::None) => Style::new(),
            (Some(colours), _) => {
                Style::new().fg(self.colour(colours.series[index % colours.series.len()]))
            }
            (None, _) => Style::new().fg(NAMED[index % NAMED.len()]),
        }
    }

    pub fn border_set(&self) -> symbols::border::Set<'static> {
        if self.icons.unicode {
            symbols::border::ROUNDED
        } else {
            ASCII_BORDER
        }
    }

    pub fn icon(&self, icon: Icon) -> &'static str {
        self.icons.get(icon)
    }
}

const ASCII_BORDER: symbols::border::Set<'static> = symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Everything a screen shows as a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Ok,
    Problem,
    Running,
    Stopped,
    Settings,
    Device,
    Vm,
    Growing,
    Shrinking,
    Steady,
    Latched,
    Host,
    Pool,
    Guest,
    Pointer,
    /// A filled cell of the pool's bar.
    Full,
    /// An empty cell of the pool's bar.
    Empty,
    /// A gauge's filled part.
    Line,
    /// A gauge's empty part.
    Rule,
}

/// Each meaning with its Unicode and its ASCII symbol: the one table.
pub const ICONS: [(Icon, &str, &str); 19] = [
    (Icon::Ok, "✓", "[ok]"),
    (Icon::Problem, "⚠", "[!!]"),
    (Icon::Running, "●", "*"),
    (Icon::Stopped, "○", "o"),
    (Icon::Settings, "⚙", "[=]"),
    (Icon::Device, "▣", "[#]"),
    (Icon::Vm, "◆", "<>"),
    (Icon::Growing, "▲", "^"),
    (Icon::Shrinking, "▼", "v"),
    (Icon::Steady, "■", "="),
    (Icon::Latched, "✖", "[X]"),
    (Icon::Host, "⌂", "[h]"),
    (Icon::Pool, "≋", "[p]"),
    (Icon::Guest, "◎", "(g)"),
    (Icon::Pointer, "▍", ">"),
    (Icon::Full, "█", "#"),
    (Icon::Empty, "━", "."),
    (Icon::Line, "━", "#"),
    (Icon::Rule, "━", "."),
];

/// Unicode symbols, or plain ASCII for a terminal or locale without UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icons {
    pub unicode: bool,
}

impl Icons {
    /// Unicode when the locale is UTF-8 and the terminal isn't dumb.
    pub fn detect(locale: Option<&str>, term: Option<&str>) -> Self {
        let utf8 = locale.is_some_and(|locale| {
            let locale = locale.to_ascii_lowercase();
            locale.contains("utf-8") || locale.contains("utf8")
        });
        Self {
            unicode: utf8 && term.is_some_and(|term| term != "dumb"),
        }
    }

    pub fn from_environment() -> Self {
        // The first of these that is set decides, as the C library reads them.
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
        Self::detect(locale.as_deref(), std::env::var("TERM").ok().as_deref())
    }

    pub fn get(self, icon: Icon) -> &'static str {
        ICONS
            .iter()
            .find(|(meaning, _, _)| *meaning == icon)
            .map_or(
                "?",
                |(_, unicode, ascii)| {
                    if self.unicode {
                        unicode
                    } else {
                        ascii
                    }
                },
            )
    }

    /// The eight-level bar set for sparklines.
    pub fn bars(self) -> symbols::bar::Set<'static> {
        if self.unicode {
            symbols::bar::NINE_LEVELS
        } else {
            symbols::bar::Set {
                full: "#",
                seven_eighths: "#",
                three_quarters: "=",
                five_eighths: "=",
                half: "-",
                three_eighths: "-",
                one_quarter: ".",
                one_eighth: ".",
                empty: " ",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_follows_the_terminal() {
        assert_eq!(
            Depth::detect(Some("xterm-256color"), None, false),
            Depth::TrueColor
        );
        assert_eq!(
            Depth::detect(Some("screen"), Some("truecolor"), false),
            Depth::TrueColor
        );
        assert_eq!(Depth::detect(Some("xterm"), None, false), Depth::Colours16);
        assert_eq!(Depth::detect(Some("linux"), None, false), Depth::Colours16);
        assert_eq!(Depth::detect(Some("dumb"), None, false), Depth::None);
        assert_eq!(
            Depth::detect(Some("xterm-256color"), None, true),
            Depth::None
        );
        assert_eq!(Depth::detect(None, None, false), Depth::None);
        // What the terminal says, asked, comes before what TERM says.
        assert_eq!(Depth::Colours16.probed(Some(true)), Depth::TrueColor);
        assert_eq!(Depth::TrueColor.probed(Some(false)), Depth::Colours256);
        assert_eq!(Depth::TrueColor.probed(None), Depth::TrueColor);
        assert_eq!(Depth::None.probed(Some(true)), Depth::None);
    }

    #[test]
    fn a_light_terminal_gets_light_before_midnight_and_darker_dim_text() {
        assert_eq!(Palette::Midnight.next(true), Palette::Light);
        assert_eq!(Palette::Midnight.next(false), Palette::Midnight256);
        let theme = Theme::new(Palette::Terminal, Depth::Colours16, Icons { unicode: true });
        assert_eq!(theme.dim().fg, Some(Color::Gray));
        assert_eq!(theme.on_light(true).dim().fg, Some(Color::DarkGray));
    }

    #[test]
    fn icons_are_ascii_without_a_utf8_locale() {
        assert!(Icons::detect(Some("en_GB.UTF-8"), Some("xterm")).unicode);
        assert!(Icons::detect(Some("C.utf8"), Some("screen")).unicode);
        assert!(!Icons::detect(Some("C"), Some("xterm")).unicode);
        assert!(!Icons::detect(None, Some("xterm")).unicode);
        assert!(!Icons::detect(Some("en_US.UTF-8"), Some("dumb")).unicode);
    }

    #[test]
    fn every_icon_has_a_unicode_and_an_ascii_symbol() {
        for (icon, unicode, ascii) in ICONS {
            assert!(!unicode.is_empty() && !ascii.is_empty(), "{icon:?}");
            assert!(ascii.is_ascii(), "{icon:?}");
            assert_eq!(Icons { unicode: true }.get(icon), unicode);
            assert_eq!(Icons { unicode: false }.get(icon), ascii);
        }
    }

    #[test]
    fn the_256_colour_midnight_is_the_nearest_of_the_256() {
        assert_eq!(nearest_256((0, 0, 0)), 16);
        assert_eq!(nearest_256((255, 255, 255)), 231);
        assert_eq!(nearest_256((0x80, 0x80, 0x80)), 244);
        // The midnight background stays a near-black, not a bright purple.
        assert!(matches!(nearest_256(MIDNIGHT.background), 16 | 232 | 233));
        let midnight = Theme::new(Palette::Midnight, Depth::TrueColor, Icons { unicode: true });
        assert_eq!(midnight.base().bg, Some(Color::Rgb(0x05, 0x03, 0x07)));
        let indexed = Theme::new(
            Palette::Midnight256,
            Depth::TrueColor,
            Icons { unicode: true },
        );
        assert!(matches!(indexed.base().bg, Some(Color::Indexed(_))));
    }

    #[test]
    fn without_colour_problems_are_bold_and_selection_reversed() {
        let theme = Theme::new(Palette::Midnight, Depth::None, Icons { unicode: false });
        assert!(theme.tone(Tone::Bad).add_modifier.contains(Modifier::BOLD));
        assert!(theme.selected().add_modifier.contains(Modifier::REVERSED));
        assert_eq!(theme.tone(Tone::Good).fg, None);
        let sixteen = Theme::new(Palette::Midnight, Depth::Colours16, Icons { unicode: true });
        assert_eq!(sixteen.tone(Tone::Bad).fg, Some(Color::Red));
        // Text sets only its colour, so a card's background shows behind it.
        let midnight = Theme::new(Palette::Midnight, Depth::TrueColor, Icons { unicode: true });
        assert_eq!(midnight.text().bg, None);
        assert_ne!(midnight.card().bg, midnight.base().bg);
    }
}
