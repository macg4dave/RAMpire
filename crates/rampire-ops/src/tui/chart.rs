//! The histories as charts: lines over time with a scale in GiB and a time
//! axis back to now, coloured by how hard pressed they are, with a strip
//! for the host's own memory against its floor and a line of figures
//! beneath: now, the average, the peak and the low over what is shown.
//!
//! Where there is no room for a chart, or no Unicode for its dots, the
//! same history is drawn as columns, each coloured by its own load.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph, Widget};

use super::theme::{Icon, Theme};
use super::widgets::panel;
use crate::dashboard::GIB;
use crate::tone::{self, Tone};
use crate::top::PoolSnapshot;
use crate::watch::VmSnapshot;

/// One line of a chart.
struct Plot {
    points: Vec<(f64, f64)>,
    style: Style,
}

/// Bytes as GiB, for the charts.
fn gib(bytes: u64) -> f64 {
    bytes as f64 / GIB as f64
}

/// A figure in GiB, short: one place below 10, none above.
fn short(value: f64) -> String {
    if value < 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}

/// A length of time, as the axis writes it: `45s`, `5m`, `2m30s`.
pub fn ago(seconds: f64) -> String {
    let seconds = seconds.round() as u64;
    match (seconds / 60, seconds % 60) {
        (0, seconds) => format!("{seconds}s"),
        (minutes, 0) => format!("{minutes}m"),
        (minutes, seconds) => format!("{minutes}m{seconds:02}s"),
    }
}

/// The top of the scale: a little above `peak`, rounded to a figure that
/// reads well (1, 1.5, 2, 2.5, 3, 4, 5, 6 or 8 times a power of ten).
pub fn ceiling(peak: f64) -> f64 {
    if peak <= 0.0 {
        return 1.0;
    }
    let raw = peak * 1.1;
    let power = 10f64.powf(raw.log10().floor());
    [1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0]
        .iter()
        .map(|step| step * power)
        .find(|value| *value >= raw)
        .unwrap_or(10.0 * power)
}

/// Each sample's place on the time axis: seconds before now, newest at 0.
fn times(count: usize, interval: f64) -> Vec<f64> {
    (0..count)
        .map(|index| -((count - 1 - index) as f64) * interval)
        .collect()
}

/// `points` split where their tone changes, each run with its tone; a run
/// starts at the last point of the one before, so the line is unbroken.
fn bands(points: &[(f64, f64)], tone_of: impl Fn(usize) -> Tone) -> Vec<(Tone, Vec<(f64, f64)>)> {
    let mut runs: Vec<(Tone, Vec<(f64, f64)>)> = Vec::new();
    for (index, point) in points.iter().enumerate() {
        let tone = tone_of(index);
        match runs.last_mut() {
            Some((last, run)) if *last == tone => run.push(*point),
            Some((_, run)) => {
                let joint = run.last().copied();
                runs.push((tone, joint.into_iter().chain([*point]).collect()));
            }
            None => runs.push((tone, vec![*point])),
        }
    }
    runs
}

/// A line chart of `plots` in `area`: GiB up to `top` on the left, time
/// back `span` seconds along the bottom.
fn line_chart(area: Rect, buf: &mut Buffer, theme: &Theme, plots: &[Plot], top: f64, span: f64) {
    let datasets: Vec<Dataset> = plots
        .iter()
        .map(|plot| {
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(plot.style)
                .data(&plot.points)
        })
        .collect();
    let x_labels = vec![
        Span::styled(format!("-{}", ago(span)), theme.dim()),
        Span::styled(format!("-{}", ago(span / 2.0)), theme.dim()),
        Span::styled("now", theme.dim()),
    ];
    let y_labels = vec![
        Span::styled("0", theme.dim()),
        Span::styled(short(top / 2.0), theme.dim()),
        Span::styled(format!("{} GiB", short(top)), theme.dim()),
    ];
    Chart::new(datasets)
        .style(theme.card())
        .legend_position(None)
        .x_axis(
            Axis::default()
                .bounds([-span, 0.0])
                .labels(x_labels)
                .style(theme.frame()),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, top])
                .labels(y_labels)
                .style(theme.frame()),
        )
        .render(area, buf);
}

/// The newest of `values` as columns `area.height` rows high, newest on
/// the right, each in the tone `tone_of` gives its sample.
fn columns(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    values: &[f64],
    top: f64,
    tone_of: impl Fn(usize) -> Tone,
) {
    let width = area.width as usize;
    let start = values.len().saturating_sub(width);
    let bars = theme.icons.bars();
    let levels = [
        bars.empty,
        bars.one_eighth,
        bars.one_quarter,
        bars.three_eighths,
        bars.half,
        bars.five_eighths,
        bars.three_quarters,
        bars.seven_eighths,
        bars.full,
    ];
    let height = area.height as usize;
    for (column, (index, value)) in values.iter().enumerate().skip(start).enumerate() {
        let x = area.x + (width - (values.len() - start)) as u16 + column as u16;
        let eighths = ((value / top.max(f64::MIN_POSITIVE)).clamp(0.0, 1.0) * (height * 8) as f64)
            .round() as usize;
        let style = theme.tone(tone_of(index));
        for row in 0..height {
            let filled = eighths.saturating_sub(row * 8).min(8);
            let y = area.bottom() - 1 - row as u16;
            buf.set_string(x, y, levels[filled], style);
        }
    }
}

/// A one-row strip of the newest `shares` (each 0 to 1), each cell as
/// high as its share and coloured by its tone, newest on the right,
/// labelled on the left: its shape says what its colour does.
fn strip(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    label: &str,
    shares: &[f64],
    tone_of: impl Fn(usize) -> Tone,
) {
    let label = format!("{label:<10}");
    buf.set_string(area.x, area.y, &label, theme.dim());
    let width = area.width.saturating_sub(label.len() as u16) as usize;
    let x0 = area.x + label.len() as u16;
    columns(
        Rect {
            x: x0,
            width: width as u16,
            ..area
        },
        buf,
        theme,
        shares,
        1.0,
        tone_of,
    );
}

/// A legend for a chart's title: each line's name in its colour.
fn legend(theme: &Theme, items: &[(String, Style)]) -> Line<'static> {
    let mark = if theme.icons.unicode { "━" } else { "-" };
    let mut spans = Vec::new();
    for (name, style) in items {
        spans.push(Span::styled(format!(" {mark}"), *style));
        spans.push(Span::styled(format!(" {name}"), theme.dim()));
    }
    spans.push(Span::styled(" ", theme.dim()));
    Line::from(spans).right_aligned()
}

/// `block` with the first of `choices` of legend that fits beside its
/// title in `width` columns, or none.
fn with_legend<'a>(
    block: Block<'a>,
    theme: &Theme,
    width: u16,
    title: &str,
    choices: &[Vec<(String, Style)>],
) -> Block<'a> {
    // The title's chip, its icon and the gaps around it.
    let used = title.chars().count() + 12;
    for items in choices {
        let line = legend(theme, items);
        if used + line.width() + 2 <= usize::from(width) {
            return block.title_top(line);
        }
    }
    block
}

/// Figures over what is shown, as one line: a label, then each figure.
fn figures<'a>(theme: &Theme, parts: &[(&str, String, Tone)]) -> Line<'a> {
    let mut spans = Vec::new();
    for (index, (label, value, tone)) in parts.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", theme.text()));
        }
        spans.push(Span::styled(format!("{label} "), theme.dim()));
        spans.push(Span::styled(
            value.clone(),
            theme
                .tone(*tone)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// The runs of samples where the guest had published, each with the index
/// of its first sample, as points of `value`.
fn published_runs(
    xs: &[f64],
    guest: &[Option<(u64, u64)>],
    value: impl Fn((u64, u64)) -> f64,
) -> Vec<(usize, Vec<(f64, f64)>)> {
    let mut runs: Vec<(usize, Vec<(f64, f64)>)> = Vec::new();
    let mut open = false;
    for (index, x) in xs.iter().enumerate() {
        match guest.get(index).copied().flatten() {
            Some(memory) => {
                if !open {
                    runs.push((index, Vec::new()));
                    open = true;
                }
                if let Some((_, run)) = runs.last_mut() {
                    run.push((*x, value(memory)));
                }
            }
            None => open = false,
        }
    }
    runs
}

/// The smallest, the mean and the largest of `values`.
fn spread(values: &[f64]) -> (f64, f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (low, values.iter().sum::<f64>() / values.len() as f64, high)
}

/// How a history panel's inside is shared: the chart or columns, a strip,
/// the figures.
fn split(inner: Rect, strip: bool) -> (Rect, Option<Rect>, Option<Rect>) {
    match inner.height {
        0 | 1 => (inner, None, None),
        2 => {
            let [graph, figures] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
            (graph, None, Some(figures))
        }
        _ if strip => {
            let [graph, strip, figures] = Layout::vertical([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            (graph, Some(strip), Some(figures))
        }
        _ => {
            let [graph, figures] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
            (graph, None, Some(figures))
        }
    }
}

/// Whether `area` takes a line chart: a few rows, and dots to draw with.
fn chart_fits(theme: &Theme, area: Rect) -> bool {
    theme.icons.unicode && area.height >= 4 && area.width >= 30
}

/// The pool over time: what is charged, coloured by how full the pool is,
/// each VM's own charge in its colour, and the pool's size; beneath, the
/// host's free memory against its floor, and the figures.
pub fn pool_history(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &PoolSnapshot) {
    let interval = snapshot.command.interval.as_secs_f64().max(0.001);
    let count = snapshot.history.len();
    let span = (count as f64 * interval).max(interval);
    // The legend as long as there is room for: each VM by name, then less.
    let pool_line = snapshot
        .command
        .pool_bytes
        .map(|_| ("pool".to_owned(), theme.dim()));
    let used = |text: &str| (text.to_owned(), theme.tone(Tone::Good));
    let mut full = vec![used("used: green, amber, red as it fills")];
    if snapshot.command.vms.len() > 1 {
        for (index, vm) in snapshot.command.vms.iter().enumerate() {
            full.push((vm.name.clone(), theme.series(index)));
        }
    }
    full.extend(pool_line.clone());
    let mut plain = vec![used("used: green, amber, red as it fills")];
    plain.extend(pool_line.clone());
    let mut brief = vec![used("used, by load")];
    brief.extend(pool_line);
    let title = format!("the pool, last {}", ago(span));
    let block = with_legend(
        panel(theme, Icon::Pool, "HISTORY", &title),
        theme,
        area.width,
        &format!("HISTORY {title}"),
        &[full, plain, brief],
    );
    let inner = block.inner(area);
    block.render(area, buf);
    if count == 0 {
        Paragraph::new(Span::styled("no samples yet", theme.dim())).render(inner, buf);
        return;
    }
    let pool = snapshot.command.pool_bytes;
    let scale = snapshot.scale().max(1);
    let used: Vec<f64> = snapshot
        .history
        .iter()
        .map(|charges| gib(charges.iter().copied().fold(0, u64::saturating_add)))
        .collect();
    let load = |index: usize| tone::load(used.get(index).copied().unwrap_or(0.0) / gib(scale));
    let floor = snapshot.command.host_floor_bytes;
    let host = &snapshot.host_history;
    let host_tone = |index: usize| {
        host.get(index)
            .map_or(Tone::Dim, |available| tone::floor(*available, floor))
    };
    let (graph, strip_area, figures_area) = split(inner, !host.is_empty());
    let (_, average, peak) = spread(&used);
    let top = ceiling(peak.max(pool.map_or(0.0, gib)));
    if chart_fits(theme, graph) {
        let xs = times(count, interval);
        let mut plots = Vec::new();
        if let Some(pool) = pool {
            plots.push(Plot {
                points: vec![(-span, gib(pool)), (0.0, gib(pool))],
                style: theme.dim(),
            });
        }
        // Each VM's own line, when there are more than one to tell apart.
        let vms = snapshot.command.vms.len();
        if vms > 1 {
            for vm in 0..vms {
                plots.push(Plot {
                    points: xs
                        .iter()
                        .zip(&snapshot.history)
                        .map(|(x, charges)| (*x, gib(charges.get(vm).copied().unwrap_or(0))))
                        .collect(),
                    style: theme.series(vm),
                });
            }
        }
        let total: Vec<(f64, f64)> = xs.iter().copied().zip(used.iter().copied()).collect();
        for (tone, points) in bands(&total, load) {
            plots.push(Plot {
                points,
                style: theme.tone(tone),
            });
        }
        line_chart(graph, buf, theme, &plots, top, span);
    } else {
        columns(graph, buf, theme, &used, gib(scale), load);
    }
    if let Some(strip_area) = strip_area {
        let most = host.iter().copied().max().unwrap_or(1).max(1);
        let shares: Vec<f64> = host
            .iter()
            .map(|available| *available as f64 / most as f64)
            .collect();
        strip(strip_area, buf, theme, "host free", &shares, host_tone);
    }
    if let Some(figures_area) = figures_area {
        let now = used.last().copied().unwrap_or(0.0);
        let mut parts = vec![
            (
                "now",
                format!("{} GiB {:.0}%", short(now), now / gib(scale) * 100.0),
                load(count - 1),
            ),
            ("avg", short(average), Tone::Value),
            ("peak", short(peak), tone::load(peak / gib(scale))),
        ];
        if let Some(low) = snapshot.session.low_free {
            parts.push((
                "least free",
                short(gib(low)),
                if low == 0 { Tone::Bad } else { Tone::Value },
            ));
        }
        if let Some(low) = host.iter().copied().min() {
            parts.push((
                "host low",
                format!("{} (floor {})", short(gib(low)), short(gib(floor))),
                tone::floor(low, floor),
            ));
        }
        let change = now - used.first().copied().unwrap_or(now);
        let sign = if change < 0.0 { "-" } else { "+" };
        parts.push((
            "change",
            format!("{sign}{}", short(change.abs())),
            if change.abs() < 0.05 {
                Tone::Dim
            } else {
                Tone::Info
            },
        ));
        Paragraph::new(figures(theme, &parts))
            .style(theme.card())
            .render(figures_area, buf);
    }
}

/// One VM over time: its device's size, what was requested and what is
/// plugged, and the guest's memory in use, coloured by how full the guest
/// is; beneath, the figures and how many resizes there were.
pub fn vm_history(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &VmSnapshot) {
    let interval = snapshot.command.interval.as_secs_f64().max(0.001);
    let count = snapshot.history.len();
    let span = (count as f64 * interval).max(interval);
    let line = |text: &str, style: Style| (text.to_owned(), style);
    let full = vec![
        line("plugged", theme.tone(Tone::Info)),
        line("requested", theme.accent()),
        line(
            "guest use: green, amber, red as it fills",
            theme.tone(Tone::Good),
        ),
        line("guest total", theme.dim()),
    ];
    let brief = vec![
        line("plugged", theme.tone(Tone::Info)),
        line("requested", theme.accent()),
        line("guest use, by load", theme.tone(Tone::Good)),
        line("total", theme.dim()),
    ];
    let least = vec![
        line("plugged", theme.tone(Tone::Info)),
        line("requested", theme.accent()),
    ];
    let title = format!("last {}", ago(span));
    let block = with_legend(
        panel(theme, Icon::Device, "HISTORY", &title),
        theme,
        area.width,
        &format!("HISTORY {title}"),
        &[full, brief, least],
    );
    let inner = block.inner(area);
    block.render(area, buf);
    if count == 0 {
        Paragraph::new(Span::styled("no device samples yet", theme.dim())).render(inner, buf);
        return;
    }
    let plugged: Vec<f64> = snapshot
        .history
        .iter()
        .map(|device| gib(device.current_bytes))
        .collect();
    let guest = &snapshot.guest_history;
    let guest_load = |index: usize| match guest.get(index).copied().flatten() {
        Some((used, total)) => tone::load(used as f64 / total.max(1) as f64),
        None => Tone::Dim,
    };
    let size = snapshot
        .history
        .iter()
        .map(|device| device.size_bytes)
        .max()
        .unwrap_or(0);
    // The scale follows what moves; the device's size is in the figures.
    let guest_peak = guest
        .iter()
        .flatten()
        .map(|(_, total)| gib(*total))
        .fold(0.0, f64::max);
    let device_peak = snapshot
        .history
        .iter()
        .map(|device| gib(device.requested_bytes.max(device.current_bytes)))
        .fold(0.0, f64::max);
    let top = ceiling(guest_peak.max(device_peak));
    let (graph, strip_area, figures_area) = split(inner, guest.iter().any(Option::is_some));
    if chart_fits(theme, graph) {
        let xs = times(count, interval);
        let mut plots = Vec::new();
        // The guest's total and its use, in runs where it published; its
        // use coloured by how full it is.
        for (_, points) in published_runs(&xs, guest, |(_, total)| gib(total)) {
            plots.push(Plot {
                points,
                style: theme.dim(),
            });
        }
        for (first, points) in published_runs(&xs, guest, |(used, _)| gib(used)) {
            for (tone, points) in bands(&points, |index| guest_load(first + index)) {
                plots.push(Plot {
                    points,
                    style: theme.tone(tone),
                });
            }
        }
        plots.push(Plot {
            points: xs
                .iter()
                .zip(&snapshot.history)
                .map(|(x, device)| (*x, gib(device.requested_bytes)))
                .collect(),
            style: theme.accent(),
        });
        plots.push(Plot {
            points: xs.iter().copied().zip(plugged.iter().copied()).collect(),
            style: theme.tone(Tone::Info),
        });
        line_chart(graph, buf, theme, &plots, top, span);
    } else {
        columns(graph, buf, theme, &plugged, top, |_| Tone::Info);
    }
    if let Some(strip_area) = strip_area {
        let shares: Vec<f64> = guest
            .iter()
            .map(|memory| memory.map_or(0.0, |(used, total)| used as f64 / total.max(1) as f64))
            .collect();
        strip(strip_area, buf, theme, "guest use", &shares, guest_load);
    }
    if let Some(figures_area) = figures_area {
        let (low, average, high) = spread(&plugged);
        let (mut grows, mut shrinks) = (0, 0);
        for pair in snapshot.history.windows(2) {
            match pair[1].requested_bytes.cmp(&pair[0].requested_bytes) {
                std::cmp::Ordering::Greater => grows += 1,
                std::cmp::Ordering::Less => shrinks += 1,
                std::cmp::Ordering::Equal => {}
            }
        }
        let mut parts = vec![
            (
                "plugged",
                format!("{} GiB", short(plugged.last().copied().unwrap_or(0.0))),
                Tone::Info,
            ),
            ("low", short(low), Tone::Value),
            ("avg", short(average), Tone::Value),
            ("high", short(high), Tone::Value),
            ("of", format!("{} GiB", short(gib(size))), Tone::Dim),
            (
                "grows",
                grows.to_string(),
                if grows > 0 { Tone::Info } else { Tone::Dim },
            ),
            (
                "shrinks",
                shrinks.to_string(),
                if shrinks > 0 { Tone::Warn } else { Tone::Dim },
            ),
        ];
        let shares: Vec<f64> = guest
            .iter()
            .flatten()
            .map(|(used, total)| *used as f64 / (*total).max(1) as f64)
            .collect();
        if let (Some(now), Some(peak)) = (
            shares.last().copied(),
            shares.iter().copied().reduce(f64::max),
        ) {
            parts.push((
                "guest",
                format!("{:.0}% peak {:.0}%", now * 100.0, peak * 100.0),
                tone::load(peak),
            ));
        }
        Paragraph::new(figures(theme, &parts))
            .style(theme.card())
            .render(figures_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::pool::tests::themes;
    use crate::tui::widgets::plain_text;

    #[test]
    fn the_scale_reads_well_and_times_count_back_to_now() {
        assert_eq!(ceiling(11.0), 15.0);
        assert_eq!(ceiling(9.0), 10.0);
        assert_eq!(ceiling(2.0), 2.5);
        assert_eq!(ceiling(0.4), 0.5);
        assert_eq!(ceiling(0.0), 1.0);
        assert_eq!(ago(45.0), "45s");
        assert_eq!(ago(300.0), "5m");
        assert_eq!(ago(150.0), "2m30s");
        assert_eq!(times(3, 2.0), [-4.0, -2.0, 0.0]);
    }

    #[test]
    fn a_line_changes_colour_where_its_tone_does_and_stays_unbroken() {
        let points = [(0.0, 1.0), (1.0, 2.0), (2.0, 9.5), (3.0, 9.6), (4.0, 1.0)];
        let runs = bands(&points, |index| {
            if (2..4).contains(&index) {
                Tone::Bad
            } else {
                Tone::Good
            }
        });
        let tones: Vec<Tone> = runs.iter().map(|(tone, _)| *tone).collect();
        assert_eq!(tones, [Tone::Good, Tone::Bad, Tone::Good]);
        // Each run after the first starts where the one before ended.
        assert_eq!(runs[1].1[0], (1.0, 2.0));
        assert_eq!(runs[2].1, [(3.0, 9.6), (4.0, 1.0)]);
    }

    #[test]
    fn the_pool_history_draws_a_chart_or_columns_with_its_figures() {
        let mut snapshot = crate::tui::pool::tests::snapshot(3);
        snapshot.host_history = vec![20 * GIB; snapshot.history.len()];
        for theme in themes() {
            for (width, height) in [(80, 3), (80, 6), (140, 12), (200, 20)] {
                let area = Rect::new(0, 0, width, height);
                let mut buf = Buffer::empty(area);
                pool_history(area, &mut buf, &theme, &snapshot);
                let text = plain_text(&buf);
                assert!(text.contains("HISTORY"), "{text}");
                if height >= 4 {
                    assert!(
                        text.contains("now") && text.contains("peak"),
                        "{width}x{height}\n{text}"
                    );
                }
                // 400 samples a second apart, as the test snapshot has.
                if height >= 12 && theme.icons.unicode {
                    assert!(
                        text.contains("GiB") && text.contains("-6m40s"),
                        "{width}x{height}\n{text}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_vm_history_counts_its_resizes_and_the_guests_use() {
        let mut snapshot = crate::tui::vm::tests::snapshot();
        let mut device = snapshot.history[0];
        snapshot.history.truncate(10);
        device.requested_bytes = 2 * GIB;
        snapshot.history.push(device);
        snapshot.guest_history = vec![Some((GIB, 2 * GIB)); snapshot.history.len()];
        let theme = themes()[0];
        let area = Rect::new(0, 0, 120, 14);
        let mut buf = Buffer::empty(area);
        vm_history(area, &mut buf, &theme, &snapshot);
        let text = plain_text(&buf);
        assert!(text.contains("grows 1"), "{text}");
        assert!(text.contains("guest 50% peak 50%"), "{text}");
        assert!(text.contains("guest use"), "{text}");
    }
}
