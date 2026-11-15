//! The TUI's screenshot for software centres and the README: the
//! dashboard drawn from a made-up host with four VMs, as an SVG of the
//! terminal's cells. Only tests build it.
//!
//! ```bash
//! cargo test -p rampire-ops -- --ignored tui::screenshot
//! rsvg-convert target/screenshots/tui.svg -o docs/screenshots/tui.png
//! ```
//!
//! The test writes `target/screenshots/tui.svg`; the PNG made from it is
//! what the metainfo names. The cells are sized for DejaVu Sans Mono, which
//! `rsvg-convert` must find.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::theme::Theme;

/// Each cell's size in the SVG, in pixels: DejaVu Sans Mono at 16 px, a
/// cell its advance (0.602 em) by its line height (1.164 em), so box
/// drawing and block elements meet the next cell's.
const FONT_SIZE: f64 = 16.0;
const CELL_WIDTH: f64 = FONT_SIZE * 0.6021;
const CELL_HEIGHT: f64 = FONT_SIZE * 1.1641;
/// The baseline below a cell's top: the font's ascent.
const BASELINE: f64 = FONT_SIZE * 0.9282;

/// A colour as SVG writes it, `fallback` for the terminal's own.
fn hex(color: Color, fallback: &str) -> String {
    // The 16 named colours as a dark terminal shows them.
    const NAMED: [&str; 16] = [
        "#1e1e2e", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#cba6f7", "#94e2d5", "#bac2de",
        "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#cba6f7", "#94e2d5", "#cdd6f4",
    ];
    let indexed = |index: u8| -> String {
        match index {
            0..=15 => NAMED[index as usize].to_owned(),
            16..=231 => {
                let index = index - 16;
                let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
                format!(
                    "#{:02x}{:02x}{:02x}",
                    level(index / 36),
                    level(index / 6 % 6),
                    level(index % 6)
                )
            }
            _ => {
                let grey = 8 + (index - 232) * 10;
                format!("#{grey:02x}{grey:02x}{grey:02x}")
            }
        }
    };
    match color {
        Color::Reset => fallback.to_owned(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(index) => indexed(index),
        Color::Black => indexed(0),
        Color::Red => indexed(1),
        Color::Green => indexed(2),
        Color::Yellow => indexed(3),
        Color::Blue => indexed(4),
        Color::Magenta => indexed(5),
        Color::Cyan => indexed(6),
        Color::Gray => indexed(7),
        Color::DarkGray => indexed(8),
        Color::LightRed => indexed(9),
        Color::LightGreen => indexed(10),
        Color::LightYellow => indexed(11),
        Color::LightBlue => indexed(12),
        Color::LightMagenta => indexed(13),
        Color::LightCyan => indexed(14),
        Color::White => indexed(15),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The buffer as an SVG: each cell's background, then its symbol at its
/// own column, so box drawing and bars line up whatever the font.
pub fn svg(buffer: &Buffer, theme: &Theme) -> String {
    let base = theme.base();
    let background = hex(base.bg.unwrap_or(Color::Reset), "#1e1e2e");
    let text = hex(base.fg.unwrap_or(Color::Reset), "#cdd6f4");
    let area = buffer.area;
    let width = f64::from(area.width) * CELL_WIDTH;
    let height = f64::from(area.height) * CELL_HEIGHT;
    let mut cells = String::new();
    let mut glyphs = String::new();
    for y in area.top()..area.bottom() {
        let mut skip = 0;
        for x in area.left()..area.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            let wide = super::widgets::unicode_width(symbol).max(1);
            skip = wide - 1;
            let reversed = cell.modifier.contains(Modifier::REVERSED);
            let (mut fg, mut bg) = (hex(cell.fg, &text), hex(cell.bg, &background));
            if reversed {
                std::mem::swap(&mut fg, &mut bg);
            }
            let left = f64::from(x - area.left()) * CELL_WIDTH;
            let top = f64::from(y - area.top()) * CELL_HEIGHT;
            if bg != background {
                cells.push_str(&format!(
                    "<rect x=\"{left:.2}\" y=\"{top:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"{bg}\"/>",
                    CELL_WIDTH * wide as f64 + 0.3,
                    CELL_HEIGHT + 0.3
                ));
            }
            if symbol.trim().is_empty() {
                continue;
            }
            let mut style = String::new();
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str(" font-weight=\"bold\"");
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                style.push_str(" font-style=\"italic\"");
            }
            if cell.modifier.contains(Modifier::DIM) {
                style.push_str(" opacity=\"0.6\"");
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                style.push_str(" text-decoration=\"underline\"");
            }
            glyphs.push_str(&format!(
                "<text x=\"{left:.2}\" y=\"{:.2}\" fill=\"{fg}\"{style}>{}</text>",
                top + BASELINE,
                escape(symbol)
            ));
        }
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" viewBox=\"0 0 {width:.1} {height:.1}\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"{background}\"/>\n\
         <g shape-rendering=\"crispEdges\">{cells}</g>\n\
         <g font-family=\"'DejaVu Sans Mono', monospace\" font-size=\"{FONT_SIZE}\" xml:space=\"preserve\">{glyphs}</g>\n\
         </svg>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone::Tone;
    use crate::top::{PoolEvent, VmSample};
    use crate::tui::jobs::Msg;
    use rampire_core::{RawTelemetryEnvelope, VirtioMemState, GIB};

    /// A host with 64 GiB and four VMs, one of them off, over the last few
    /// minutes.
    fn host() -> crate::top::PoolSnapshot {
        let mut snapshot = crate::tui::pool::tests::snapshot(4);
        let names = ["web", "db", "build", "mail"];
        let maximum = [8 * GIB, 16 * GIB, 12 * GIB, 4 * GIB];
        for (vm, (name, max)) in snapshot
            .command
            .vms
            .iter_mut()
            .zip(names.iter().zip(maximum))
        {
            vm.name = (*name).to_owned();
            vm.max_ram = max;
            vm.min_ram = 2 * GIB;
        }
        snapshot.command.vms[1].priority = 200;
        snapshot.command.pool_bytes = Some(24 * GIB);
        let text = include_str!("../../../../fixtures/publication.json");
        let guest = |name: &str, total: u64, available: u64| {
            let mut value: serde_json::Value = serde_json::from_str(text).unwrap();
            value["vm_name"] = name.into();
            value["memory"]["physical_total_bytes"] = total.into();
            value["memory"]["physical_available_bytes"] = available.into();
            serde_json::from_value::<RawTelemetryEnvelope>(value).unwrap()
        };
        let device = |size: u64, requested: u64, current: u64| {
            Some(VirtioMemState {
                size_bytes: size,
                block_size_bytes: 2 << 20,
                requested_bytes: requested,
                current_bytes: current,
            })
        };
        snapshot.vms = vec![
            VmSample {
                device: device(6 * GIB, 4 * GIB, 3 * GIB),
                guest: Some(guest("web", 5 * GIB, GIB)),
            },
            VmSample {
                device: device(14 * GIB, 7 * GIB, 7 * GIB),
                guest: Some(guest("db", 9 * GIB, 3 * GIB)),
            },
            VmSample {
                device: device(10 * GIB, GIB, 2 * GIB),
                guest: Some(guest("build", 4 * GIB, 3 * GIB)),
            },
            VmSample {
                device: None,
                guest: None,
            },
        ];
        // A build that came and went, a database that grew and stayed, a
        // web server that is growing now.
        let samples = 400;
        snapshot.history = (0..samples)
            .map(|step| {
                let t = step as f64 / samples as f64;
                let build = (6.0 * (-(t - 0.45).powi(2) / 0.01).exp()).max(1.0);
                let db = 3.0 + 4.0 / (1.0 + (-(t - 0.3) * 20.0).exp());
                let web = 1.0 + 3.0 * t.powi(3);
                [web, db, build, 0.0]
                    .iter()
                    .map(|gib| (gib * GIB as f64) as u64)
                    .collect()
            })
            .collect();
        snapshot.host_history = snapshot
            .history
            .iter()
            .map(|charges| 40 * GIB - charges.iter().sum::<u64>())
            .collect();
        snapshot.host.available = 28 * GIB;
        let event = |time: &str, vm: &str, text: &str, tone: Tone| PoolEvent {
            time: time.to_owned(),
            vm: vm.to_owned(),
            text: text.to_owned(),
            tone,
        };
        snapshot.events = Ok(vec![
            event(
                "10:42:18",
                "web",
                "grow +1024M: memory is short",
                Tone::Info,
            ),
            event(
                "10:41:02",
                "build",
                "shrink -2048M: memory left idle",
                Tone::Good,
            ),
            event(
                "10:39:47",
                "build",
                "shrink -2048M: memory left idle",
                Tone::Good,
            ),
            event("10:36:30", "db", "grow +2048M: memory is short", Tone::Info),
            event(
                "10:35:12",
                "build",
                "grow +4096M: memory is short",
                Tone::Info,
            ),
            event("10:35:09", "web", "held back: db comes first", Tone::Warn),
        ]);
        snapshot
    }

    #[test]
    fn a_buffer_draws_as_svg_cells() {
        let theme = crate::tui::pool::tests::themes()[0];
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "a<▉", theme.base());
        let svg = svg(&buf, &theme);
        assert!(svg.starts_with("<svg"), "{svg}");
        assert!(
            svg.contains(">a</text>") && svg.contains(">&lt;</text>"),
            "{svg}"
        );
        assert!(svg.contains(">▉</text>"), "{svg}");
    }

    /// Write `to` over the first `from` in row `y`, each cell keeping its
    /// style; what is left of a longer `from` becomes spaces.
    fn replace_in_row(buf: &mut Buffer, y: u16, from: &str, to: &str) {
        let row: Vec<String> = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_owned())
            .collect();
        let wanted: Vec<String> = from.chars().map(String::from).collect();
        let Some(start) = row.windows(wanted.len()).position(|cells| cells == wanted) else {
            return;
        };
        let mut new = to.chars().map(String::from).collect::<Vec<_>>();
        new.resize(wanted.len().max(new.len()), " ".to_owned());
        for (offset, symbol) in new.iter().enumerate() {
            let x = start as u16 + offset as u16;
            if x < buf.area.width {
                buf[(x, y)].set_symbol(symbol);
            }
        }
    }

    /// Writes target/screenshots/tui.svg; see the module's documentation.
    #[test]
    #[ignore = "writes the screenshot for docs/screenshots/tui.png"]
    fn screenshot() {
        let (mut app, _rx) = crate::tui::app::tests::app();
        app.on_msg(Msg::Pool(Ok(host())));
        app.on_msg(Msg::Inventory(Ok(crate::reports::VmListReport {
            schema: crate::reports::VmListSchema,
            version: 1,
            vms: Vec::new(),
        })));
        app.controller = Some(crate::reports::ControllerUnit {
            unit: "rampire.service".to_owned(),
            active_state: "active".to_owned(),
            sub_state: "running".to_owned(),
            unit_file_state: "enabled".to_owned(),
        });
        // Tall enough for the history chart, short enough that the list keeps
        // the whole width, without a VM's details beside it.
        let area = Rect::new(0, 0, 140, 40);
        let mut buf = Buffer::empty(area);
        crate::tui::views::render(area, &mut buf, &app);
        // The header names the machine the test ran on and its clock: not
        // in a published picture.
        let host = crate::dashboard::host_name();
        let host = host.split('.').next().unwrap_or(&host).to_owned();
        replace_in_row(&mut buf, 0, &host, "kvm-host");
        replace_in_row(&mut buf, 0, &crate::dashboard::clock(), "10:42:30");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/screenshots/tui.svg");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, svg(&buf, &app.theme)).unwrap();
        println!("{}", crate::tui::widgets::plain_text(&buf));
    }
}
