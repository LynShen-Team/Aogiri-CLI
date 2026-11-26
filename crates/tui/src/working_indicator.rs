//! The row that says a turn is running: a small dot glyph, the label with a
//! soft light passing over it, and the time since the turn started —
//! `⢎⡱ 工作中 41 秒` (`Working 41s`).
//!
//! The glyph is two braille cells, a 4×4 grid of dots: a sphere of dots
//! turning on a tilted axis that, once a cycle, gathers into the LynShen
//! mark (two trunks and the light between them) and scatters back. Each
//! frame is a pure function of the time since the turn started, so a given
//! moment always draws the same row, and a frame costs a few dozen float
//! operations.
//!
//! Fallbacks: without colour (`NO_COLOR`) the dots still turn and the label
//! stays plain; 16 colours sweep the label with bold; where the console font
//! lacks braille (the legacy Windows console) the glyph is the ASCII
//! spinner; without motion (`TERM=dumb`, `CI`, output not a terminal) the
//! row stands still: the mark, the plain label and the seconds.

use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::lang::{self, t};
use crate::theme::{mix, ColorDepth, Rgb, Theme};

/// Redraw interval while the row moves (about 12 frames a second).
pub(crate) const FRAME_INTERVAL: Duration = Duration::from_millis(83);
/// Redraw interval for a still row: only the seconds change.
pub(crate) const STILL_INTERVAL: Duration = Duration::from_millis(500);

/// Dot columns and rows of the glyph (two braille cells).
const GRID: usize = 4;
/// Dots on the sphere.
const PARTICLES: usize = 11;
/// One turn of the sphere.
const SPIN_MS: f32 = 4_500.0;
/// The spin axis leans toward the viewer, so dots also drift up and down.
const TILT: f32 = 0.42;
/// A slow roll about the line of sight: the turning reads as a swirl.
const ROLL: f32 = 0.30;
const ROLL_MS: f32 = 11_000.0;
/// Dots further back than this are hidden behind the sphere.
const HIDDEN_BELOW: f32 = -0.12;
/// Every cycle the dots gather into the mark, starting at `GATHER_AT_MS`:
/// they move in, hold the mark, and scatter back (1.2 s in all).
const CYCLE_MS: u64 = 9_000;
const GATHER_AT_MS: u64 = 7_800;
const GATHER_MS: u64 = 350;
const HOLD_MS: u64 = 500;
/// The LynShen mark (the welcome logo's two trunks and the light falling
/// between them) on the 4×4 grid, as (column, row).
const MARK: [(usize, usize); 9] = [
    (0, 1),
    (0, 2),
    (0, 3),
    (1, 0),
    (2, 1),
    (3, 0),
    (3, 1),
    (3, 2),
    (3, 3),
];
/// The spinner where the console font has no braille.
const ASCII_FRAMES: [&str; 4] = ["-", "\\", "|", "/"];
const ASCII_FRAME_MS: u64 = 100;

/// The light passes over the label in `SWEEP_MS`, then rests for `REST_MS`.
const SWEEP_MS: u64 = 1_600;
const REST_MS: u64 = 600;
/// Half-width of the light, in columns.
const SWEEP_SPREAD: f32 = 2.2;
/// How far the label's resting tone sits from the background toward the
/// text colour; the light brings it the rest of the way.
const LABEL_REST: f32 = 0.66;

/// What the row says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    Working,
    Compacting,
    Reconnecting {
        attempt: usize,
    },
    /// The reply stopped streaming for a while.
    Waiting,
}

impl Phase {
    pub(crate) fn label(&self) -> String {
        match self {
            Phase::Working => t("工作中", "Working").to_string(),
            Phase::Compacting => t("压缩上下文", "Compacting context").to_string(),
            Phase::Reconnecting { attempt } => match lang::lang() {
                lang::Lang::Zh => format!("重连中（第 {attempt} 次）"),
                lang::Lang::En => format!("Reconnecting (attempt {attempt})"),
            },
            Phase::Waiting => t("等待响应", "Waiting for the reply").to_string(),
        }
    }
}

/// A running turn as the row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Progress {
    pub(crate) phase: Phase,
    /// Time since the turn started: the counter, and the animation clock.
    pub(crate) elapsed: Duration,
}

/// Whether the row may move: not when output is not a terminal, on a dumb
/// terminal, or under CI.
pub(crate) fn motion_allowed(var: impl Fn(&str) -> Option<String>, is_terminal: bool) -> bool {
    if !is_terminal {
        return false;
    }
    let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    if set("TERM").is_some_and(|term| term.trim().eq_ignore_ascii_case("dumb")) {
        return false;
    }
    let ci = set("CI")
        .is_some_and(|ci| !matches!(ci.trim().to_ascii_lowercase().as_str(), "0" | "false"));
    !ci
}

/// How soon the row needs drawing again.
pub(crate) fn redraw_interval(theme: &Theme) -> Duration {
    if theme.animate {
        FRAME_INTERVAL
    } else {
        STILL_INTERVAL
    }
}

/// Elapsed turn time: `41 秒`, `1 分 05 秒`, `1 时 02 分` (`41s`, `1m 05s`, `1h 02m`).
pub(crate) fn format_elapsed(secs: u64) -> String {
    let zh = lang::is_zh();
    if secs < 60 {
        return if zh {
            format!("{secs} 秒")
        } else {
            format!("{secs}s")
        };
    }
    let (minutes, seconds) = (secs / 60, secs % 60);
    if minutes < 60 {
        return if zh {
            format!("{minutes} 分 {seconds:02} 秒")
        } else {
            format!("{minutes}m {seconds:02}s")
        };
    }
    let (hours, minutes) = (minutes / 60, minutes % 60);
    if zh {
        format!("{hours} 时 {minutes:02} 分")
    } else {
        format!("{hours}h {minutes:02}m")
    }
}

/// The terminal's own text colour at full strength: the row's line kind
/// (`UiKind::Status`) is dim underneath, and the label is not.
fn plain() -> Style {
    Style::new().fg(Color::Reset).remove_modifier(Modifier::DIM)
}

/// The row: the content padding, the glyph, the label and the elapsed time.
pub(crate) fn line(theme: &Theme, progress: &Progress) -> Line<'static> {
    let ms = progress.elapsed.as_millis().min(u64::MAX as u128) as u64;
    let waiting = progress.phase == Phase::Waiting;
    let mut spans = vec![Span::raw("  ")];
    spans.extend(glyph_spans(theme, ms, waiting));
    spans.push(Span::raw(" "));
    let label = progress.phase.label();
    if waiting {
        spans.push(Span::styled(label, theme.warn()));
    } else if theme.animate {
        spans.extend(sweep_spans(theme, &label, ms));
    } else {
        spans.push(Span::styled(label, plain()));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format_elapsed(progress.elapsed.as_secs()),
        theme.dim(),
    ));
    Line::from(spans)
}

/// One frame of the glyph: the dots of each braille cell and how near the
/// viewer they sit (0 far … 1 near), which sets their brightness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GlyphFrame {
    /// Braille dot bits per cell (the character is U+2800 + bits).
    pub(crate) cells: [u8; 2],
    pub(crate) shade: [f32; 2],
}

impl GlyphFrame {
    /// The two cells as text; a cell without dots is a space.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        self.cells.iter().map(|bits| braille(*bits)).collect()
    }

    /// The lit dots as (column, row), for tests.
    #[cfg(test)]
    pub(crate) fn dots(&self) -> Vec<(usize, usize)> {
        let mut dots = Vec::new();
        for row in 0..GRID {
            for column in 0..GRID {
                if self.cells[column / 2] & dot_bit(column % 2, row) != 0 {
                    dots.push((column, row));
                }
            }
        }
        dots
    }
}

/// The mark as a still glyph.
pub(crate) fn mark_frame() -> GlyphFrame {
    let mut frame = GlyphFrame {
        cells: [0; 2],
        shade: [1.0; 2],
    };
    for (column, row) in MARK {
        frame.cells[column / 2] |= dot_bit(column % 2, row);
    }
    frame
}

/// The glyph `ms` into the turn.
pub(crate) fn glyph_frame(ms: u64) -> GlyphFrame {
    let points = sphere(ms);
    let gather = gather_amount(ms);
    let assigned = (gather > 0.0).then(|| {
        let start = ms - ms % CYCLE_MS + GATHER_AT_MS;
        assign_mark(&sphere(start))
    });

    let mut cells = [0u8; 2];
    let mut shade_sum = [0f32; 2];
    let mut shade_count = [0u32; 2];
    for (index, point) in points.iter().enumerate() {
        let (mut column, mut row) = (point.column, point.row);
        // 0 at the sphere's visible edge … 1 facing the viewer.
        let mut shade = ((point.depth - HIDDEN_BELOW) / (1.0 - HIDDEN_BELOW)).clamp(0.0, 1.0);
        let mut visible = point.depth > HIDDEN_BELOW;
        if let Some(assigned) = &assigned {
            match assigned[index] {
                Some((target_column, target_row)) => {
                    // Exact at full gather: the mark, at full brightness.
                    column = column * (1.0 - gather) + target_column as f32 * gather;
                    row = row * (1.0 - gather) + target_row as f32 * gather;
                    shade = shade * (1.0 - gather) + gather;
                    visible |= gather > 0.15;
                }
                None => visible &= gather < 0.4,
            }
        }
        if !visible {
            continue;
        }
        let column = column.round().clamp(0.0, (GRID - 1) as f32) as usize;
        let row = row.round().clamp(0.0, (GRID - 1) as f32) as usize;
        let cell = column / 2;
        cells[cell] |= dot_bit(column % 2, row);
        shade_sum[cell] += shade;
        shade_count[cell] += 1;
    }
    let shade = [0, 1].map(|cell| {
        if shade_count[cell] == 0 {
            1.0
        } else {
            shade_sum[cell] / shade_count[cell] as f32
        }
    });
    GlyphFrame { cells, shade }
}

/// A dot of the sphere on the grid: fractional column and row, and depth
/// (-1 far side … 1 facing the viewer).
#[derive(Debug, Clone, Copy)]
struct Point {
    column: f32,
    row: f32,
    depth: f32,
}

/// The sphere's dots `ms` into the turn: spread evenly (a Fibonacci
/// lattice), turned about the tilted axis, rolled, and projected.
fn sphere(ms: u64) -> [Point; PARTICLES] {
    use std::f32::consts::{PI, TAU};
    let golden = PI * (3.0 - 5f32.sqrt());
    let spin = TAU * ((ms as f64 % SPIN_MS as f64) as f32 / SPIN_MS);
    let roll = ROLL * (TAU * ((ms as f64 % ROLL_MS as f64) as f32 / ROLL_MS)).sin();
    let (spin_sin, spin_cos) = spin.sin_cos();
    let (tilt_sin, tilt_cos) = TILT.sin_cos();
    let (roll_sin, roll_cos) = roll.sin_cos();
    let half = GRID as f32 / 2.0;
    std::array::from_fn(|index| {
        let y = 1.0 - 2.0 * (index as f32 + 0.5) / PARTICLES as f32;
        let radius = (1.0 - y * y).max(0.0).sqrt();
        let (lon_sin, lon_cos) = (index as f32 * golden).sin_cos();
        let (x, z) = (radius * lon_cos, radius * lon_sin);
        // Spin about the vertical axis, lean it toward the viewer, roll.
        let (x, z) = (x * spin_cos + z * spin_sin, z * spin_cos - x * spin_sin);
        let (y, z) = (y * tilt_cos - z * tilt_sin, y * tilt_sin + z * tilt_cos);
        let (x, y) = (x * roll_cos - y * roll_sin, x * roll_sin + y * roll_cos);
        Point {
            column: (x + 1.0) * half - 0.5,
            row: (1.0 - y) * half - 0.5,
            depth: z,
        }
    })
}

/// How far the dots have gathered into the mark (0 sphere … 1 mark).
fn gather_amount(ms: u64) -> f32 {
    let at = ms % CYCLE_MS;
    if at < GATHER_AT_MS {
        return 0.0;
    }
    let local = at - GATHER_AT_MS;
    let progress = if local < GATHER_MS {
        local as f32 / GATHER_MS as f32
    } else if local < GATHER_MS + HOLD_MS {
        1.0
    } else if local < 2 * GATHER_MS + HOLD_MS {
        1.0 - (local - GATHER_MS - HOLD_MS) as f32 / GATHER_MS as f32
    } else {
        0.0
    };
    smoothstep(progress)
}

fn smoothstep(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

/// Which dot goes to which point of the mark: each point takes the nearest
/// free dot, preferring dots on the near side.
fn assign_mark(points: &[Point; PARTICLES]) -> [Option<(usize, usize)>; PARTICLES] {
    let mut assigned = [None; PARTICLES];
    for target in MARK {
        let cost = |point: &Point| {
            let dc = point.column - target.0 as f32;
            let dr = point.row - target.1 as f32;
            dc * dc + dr * dr + if point.depth > 0.0 { 0.0 } else { 2.0 }
        };
        let nearest = (0..PARTICLES)
            .filter(|index| assigned[*index].is_none())
            .min_by(|a, b| cost(&points[*a]).total_cmp(&cost(&points[*b])));
        if let Some(index) = nearest {
            assigned[index] = Some(target);
        }
    }
    assigned
}

/// The braille bit of a dot: `side` 0 is the cell's left column.
fn dot_bit(side: usize, row: usize) -> u8 {
    const LEFT: [u8; 4] = [0x01, 0x02, 0x04, 0x40];
    const RIGHT: [u8; 4] = [0x08, 0x10, 0x20, 0x80];
    if side == 0 {
        LEFT[row]
    } else {
        RIGHT[row]
    }
}

fn braille(bits: u8) -> char {
    if bits == 0 {
        ' '
    } else {
        char::from_u32(0x2800 + bits as u32).unwrap_or(' ')
    }
}

/// The glyph as styled spans: two braille cells, or the ASCII spinner.
fn glyph_spans(theme: &Theme, ms: u64, waiting: bool) -> Vec<Span<'static>> {
    if !theme.rich_glyphs {
        let frame = if theme.animate {
            ASCII_FRAMES[(ms / ASCII_FRAME_MS % ASCII_FRAMES.len() as u64) as usize]
        } else {
            "*"
        };
        return vec![Span::styled(frame, glyph_style(theme, 1.0, waiting))];
    }
    let frame = if theme.animate {
        glyph_frame(ms)
    } else {
        mark_frame()
    };
    frame
        .cells
        .iter()
        .zip(frame.shade)
        .map(|(bits, shade)| {
            Span::styled(
                braille(*bits).to_string(),
                glyph_style(theme, shade, waiting),
            )
        })
        .collect()
}

/// The glyph's colour: the accent (attention while waiting), stepped toward
/// the background for dots further back where the background is known.
fn glyph_style(theme: &Theme, shade: f32, waiting: bool) -> Style {
    let (tone, ansi) = if waiting {
        (theme.warn_rgb(), Color::Yellow)
    } else {
        (theme.accent_rgb(), Color::Cyan)
    };
    let rgb = match theme.background {
        Some(background) => mix(background, tone, 0.4 + 0.6 * shade.clamp(0.0, 1.0)),
        None => tone,
    };
    match theme.color(rgb, ansi) {
        Some(color) => Style::new().fg(color),
        None => plain(),
    }
}

/// How bright the light is on the label column `column` (its centre),
/// `ms` into the turn: 0 at rest … 1 under the light.
pub(crate) fn sweep_light(ms: u64, column: f32, label_width: f32) -> f32 {
    let at = ms % (SWEEP_MS + REST_MS);
    if at >= SWEEP_MS {
        return 0.0;
    }
    let travel = label_width + 4.0 * SWEEP_SPREAD;
    let centre = -2.0 * SWEEP_SPREAD + travel * at as f32 / SWEEP_MS as f32;
    let distance = (column - centre) / SWEEP_SPREAD;
    (-distance * distance).exp()
}

/// The label one span per character, each lit by the passing light.
fn sweep_spans(theme: &Theme, label: &str, ms: u64) -> Vec<Span<'static>> {
    let label_width = unicode_width::UnicodeWidthStr::width(label) as f32;
    let mut column = 0.0f32;
    label
        .chars()
        .map(|ch| {
            let width = ch.width().unwrap_or(0) as f32;
            let light = sweep_light(ms, column + width / 2.0, label_width);
            column += width;
            Span::styled(ch.to_string(), sweep_style(theme, light))
        })
        .collect()
}

/// A label character's style under `light`: a blend from the resting tone
/// to the text colour (truecolor, the 256 greys), bold under the light (16
/// colours), or plain (no colour).
pub(crate) fn sweep_style(theme: &Theme, light: f32) -> Style {
    match theme.depth {
        ColorDepth::None => plain(),
        ColorDepth::Ansi16 => {
            if light >= 0.5 {
                plain().add_modifier(Modifier::BOLD)
            } else {
                plain()
            }
        }
        ColorDepth::Ansi256 | ColorDepth::TrueColor => match theme.background {
            Some(background) => {
                let text = text_guess(theme);
                let rest = mix(background, text, LABEL_REST);
                let rgb = mix(rest, text, light.clamp(0.0, 1.0));
                match theme.color(rgb, Color::Reset) {
                    Some(color) => Style::new().fg(color),
                    None => plain(),
                }
            }
            // The text colour is unknown: dim at rest, the terminal's own
            // foreground under the light.
            None if light >= 0.5 => plain(),
            None => theme.dim(),
        },
    }
}

/// The text colour assumed for the label's blend on a known background.
fn text_guess(theme: &Theme) -> Rgb {
    if theme.is_dark().unwrap_or(true) {
        (236, 232, 224)
    } else {
        (28, 26, 23)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::{with_lang, Lang};
    use unicode_width::UnicodeWidthStr;

    const DARK: Theme = Theme::new(ColorDepth::TrueColor, Some((22, 21, 19)));

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn elapsed_reads_seconds_then_minutes_then_hours() {
        assert_eq!(format_elapsed(0), "0 秒");
        assert_eq!(format_elapsed(41), "41 秒");
        assert_eq!(format_elapsed(59), "59 秒");
        assert_eq!(format_elapsed(60), "1 分 00 秒");
        assert_eq!(format_elapsed(65), "1 分 05 秒");
        assert_eq!(format_elapsed(3_599), "59 分 59 秒");
        assert_eq!(format_elapsed(3_600), "1 时 00 分");
        assert_eq!(format_elapsed(3_720), "1 时 02 分");
        with_lang(Lang::En, || {
            assert_eq!(format_elapsed(41), "41s");
            assert_eq!(format_elapsed(65), "1m 05s");
            assert_eq!(format_elapsed(3_720), "1h 02m");
        });
    }

    #[test]
    fn row_reads_glyph_label_elapsed() {
        let row = line(
            &DARK,
            &Progress {
                phase: Phase::Working,
                elapsed: Duration::from_secs(41),
            },
        );
        let text = text_of(&row);
        assert!(text.starts_with("  "));
        assert!(text.ends_with(" 工作中 41 秒"), "{text}");
        // Padding, two glyph cells, one space, then the label.
        assert_eq!(
            UnicodeWidthStr::width(&text[..text.find("工作中").unwrap()]),
            5
        );
        with_lang(Lang::En, || {
            let row = line(
                &DARK,
                &Progress {
                    phase: Phase::Working,
                    elapsed: Duration::from_secs(65),
                },
            );
            assert!(text_of(&row).ends_with(" Working 1m 05s"));
        });
    }

    #[test]
    fn glyph_frames_are_deterministic_and_two_cells_wide() {
        for ms in (0..20_000).step_by(83) {
            let frame = glyph_frame(ms);
            assert_eq!(frame, glyph_frame(ms));
            assert_eq!(UnicodeWidthStr::width(frame.text().as_str()), 2);
            assert!(frame.shade.iter().all(|shade| (0.0..=1.0).contains(shade)));
        }
    }

    #[test]
    fn sphere_keeps_a_handful_of_dots_and_moves_gently() {
        let mut previous = glyph_frame(0);
        let mut changed = 0;
        let frames = (1..GATHER_AT_MS / 83).map(|step| step * 83);
        for ms in frames.clone() {
            let frame = glyph_frame(ms);
            let lit = frame.dots().len();
            assert!((3..=10).contains(&lit), "{lit} dots at {ms} ms");
            let before: std::collections::HashSet<_> = previous.dots().into_iter().collect();
            let after: std::collections::HashSet<_> = frame.dots().into_iter().collect();
            changed += before.symmetric_difference(&after).count();
            previous = frame;
        }
        // About two dots change a frame: it turns, it does not flicker.
        let mean = changed as f32 / frames.count() as f32;
        assert!(mean > 0.5 && mean < 3.5, "{mean} dots change per frame");
    }

    #[test]
    fn dots_gather_into_the_mark_once_a_cycle() {
        let mark = mark_frame();
        assert_eq!(mark.text(), "⡎⢺");
        let hold = GATHER_AT_MS + GATHER_MS + HOLD_MS / 2;
        for cycle in 0..3 {
            let frame = glyph_frame(cycle * CYCLE_MS + hold);
            assert_eq!(frame.cells, mark.cells, "cycle {cycle}");
            assert_eq!(frame.shade, [1.0, 1.0]);
        }
        // Before the gather and after the scatter the sphere shows.
        assert_eq!(gather_amount(GATHER_AT_MS - 1), 0.0);
        assert_eq!(gather_amount(GATHER_AT_MS + 2 * GATHER_MS + HOLD_MS), 0.0);
        assert_eq!(gather_amount(CYCLE_MS), 0.0);
    }

    #[test]
    fn light_crosses_the_label_then_rests() {
        let width = 6.0;
        // Starts off the label's left, ends off its right.
        assert!(sweep_light(0, 1.0, width) < 0.01);
        assert!(sweep_light(SWEEP_MS - 1, 5.0, width) < 0.01);
        // Mid-pass the middle character is lit.
        assert!(sweep_light(SWEEP_MS / 2, 3.0, width) > 0.9);
        // Left before right.
        let peak_at = |column: f32| {
            (0..SWEEP_MS)
                .step_by(10)
                .max_by(|a, b| {
                    sweep_light(*a, column, width).total_cmp(&sweep_light(*b, column, width))
                })
                .unwrap()
        };
        assert!(peak_at(1.0) < peak_at(3.0) && peak_at(3.0) < peak_at(5.0));
        // Then a rest with no light at all.
        for ms in SWEEP_MS..SWEEP_MS + REST_MS {
            assert_eq!(sweep_light(ms, 3.0, width), 0.0);
        }
    }

    #[test]
    fn label_light_follows_the_colour_depth() {
        let dark = DARK;
        let rest = sweep_style(&dark, 0.0).fg;
        let lit = sweep_style(&dark, 1.0).fg;
        assert!(matches!(rest, Some(Color::Rgb(..))));
        assert_ne!(rest, lit);
        let light_theme = Theme::new(ColorDepth::TrueColor, Some((251, 250, 247)));
        let (Some(Color::Rgb(r, _, _)), Some(Color::Rgb(l, _, _))) = (
            sweep_style(&light_theme, 0.0).fg,
            sweep_style(&light_theme, 1.0).fg,
        ) else {
            panic!("truecolor blends");
        };
        // On a light background the light darkens the text.
        assert!(l < r);
        let greys = Theme::new(ColorDepth::Ansi256, Some((22, 21, 19)));
        assert!(matches!(
            sweep_style(&greys, 0.3).fg,
            Some(Color::Indexed(_))
        ));
        let ansi = Theme::new(ColorDepth::Ansi16, Some((22, 21, 19)));
        assert!(sweep_style(&ansi, 1.0)
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(sweep_style(&ansi, 0.0), plain());
        let none = Theme::new(ColorDepth::None, None);
        assert_eq!(sweep_style(&none, 1.0), plain());
        // Unknown background: dim at rest, the terminal's colour under the light.
        let unknown = Theme::new(ColorDepth::TrueColor, None);
        assert_eq!(sweep_style(&unknown, 0.0), unknown.dim());
        assert_eq!(sweep_style(&unknown, 1.0), plain());
    }

    #[test]
    fn no_colour_keeps_the_dots_turning_without_colour() {
        let none = Theme::new(ColorDepth::None, None);
        let progress = |ms| Progress {
            phase: Phase::Working,
            elapsed: Duration::from_millis(ms),
        };
        let first = line(&none, &progress(0));
        assert!(first
            .spans
            .iter()
            .all(|span| matches!(span.style.fg, None | Some(Color::Reset))));
        let glyphs: std::collections::HashSet<String> = (0..40)
            .map(|step| {
                text_of(&line(&none, &progress(step * 83)))[2..]
                    .chars()
                    .take(2)
                    .collect()
            })
            .collect();
        assert!(glyphs.len() > 4, "the glyph moves");
        assert!(text_of(&first).ends_with(" 工作中 0 秒"));
    }

    #[test]
    fn legacy_console_falls_back_to_the_ascii_spinner() {
        let console = Theme {
            rich_glyphs: false,
            ..Theme::new(ColorDepth::TrueColor, None)
        };
        for step in 0..12 {
            let row = line(
                &console,
                &Progress {
                    phase: Phase::Working,
                    elapsed: Duration::from_millis(step * 83),
                },
            );
            let text = text_of(&row);
            assert!(text.is_char_boundary(3));
            assert!(ASCII_FRAMES.contains(&&text[2..3]), "{text}");
            assert!(text[..text.find("工作中").unwrap()].is_ascii());
        }
    }

    #[test]
    fn still_row_shows_the_mark_and_plain_label() {
        let still = Theme {
            animate: false,
            ..DARK
        };
        let row = |ms| {
            line(
                &still,
                &Progress {
                    phase: Phase::Working,
                    elapsed: Duration::from_millis(ms),
                },
            )
        };
        assert_eq!(text_of(&row(0)), "  ⡎⢺ 工作中 0 秒");
        // Nothing but the seconds changes.
        assert_eq!(text_of(&row(900)), text_of(&row(0)));
        assert_eq!(row(300).spans, row(0).spans);
        assert_eq!(text_of(&row(5_000)), "  ⡎⢺ 工作中 5 秒");
        assert_eq!(redraw_interval(&still), STILL_INTERVAL);
        assert_eq!(redraw_interval(&DARK), FRAME_INTERVAL);
    }

    #[test]
    fn waiting_row_uses_the_attention_tone() {
        let row = line(
            &DARK,
            &Progress {
                phase: Phase::Waiting,
                elapsed: Duration::from_secs(20),
            },
        );
        assert!(text_of(&row).ends_with(" 等待响应 20 秒"));
        let label = row
            .spans
            .iter()
            .find(|span| span.content == "等待响应")
            .expect("the label is one span");
        assert_eq!(label.style, DARK.warn());
    }

    #[test]
    fn motion_needs_a_real_terminal_outside_ci() {
        assert!(motion_allowed(env(&[("TERM", "xterm-256color")]), true));
        assert!(!motion_allowed(env(&[("TERM", "xterm-256color")]), false));
        assert!(!motion_allowed(env(&[("TERM", "dumb")]), true));
        assert!(!motion_allowed(env(&[("CI", "true")]), true));
        assert!(!motion_allowed(env(&[("CI", "1")]), true));
        assert!(motion_allowed(env(&[("CI", "false")]), true));
        assert!(motion_allowed(env(&[("CI", "")]), true));
    }
}
