//! The row that says a turn is running: a small dot glyph whose motion
//! follows what the turn is doing, the label with a soft light passing over
//! it, and the time since the turn started — `⢎⡱ 工作中 41 秒`
//! (`Working 41s`).
//!
//! The glyph is two braille cells, a 4×4 grid of dots. Connecting lights the
//! ring dot by dot; thinking turns a sphere of dots; writing the reply
//! streams dots left to right; a command or another tool sends a short
//! trail around the ring; reading and searching move a scan line down;
//! editing types rows of dots and clears them; waiting (for the user's
//! approval, or a reply that stalled) breathes; compacting draws the dots
//! to the centre and spreads them again. A new motion shows once it has
//! been asked for 400 ms without a change of mind (`MotionTrack`), and
//! dissolves in over a quarter second. Each frame is a pure function of the
//! motion and its clock.
//!
//! Fallbacks: without colour (`NO_COLOR`) the dots still move and the label
//! stays plain; 16 colours sweep the label with bold; where the console font
//! lacks braille (the legacy Windows console) the glyph is the ASCII
//! spinner; without motion (`TERM=dumb`, `CI`, output not a terminal) the
//! row stands still: the ring, the plain label and the seconds.

use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::lang::{self, t};
use crate::theme::{mix, ColorDepth, Rgb, Theme};

/// Redraw interval while the row moves (about 12 frames a second).
pub(crate) const FRAME_INTERVAL: Duration = Duration::from_millis(83);
/// Redraw interval for a still row: only the seconds change.
pub(crate) const STILL_INTERVAL: Duration = Duration::from_millis(500);
/// A new motion shows once it has been asked for this long: quicker flips
/// (a tool that ends at once, a reply between two tools) never show.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(400);
/// A new motion dissolves in over this long.
pub(crate) const BLEND_MS: u64 = 240;

/// Dot columns and rows of the glyph (two braille cells).
const GRID: usize = 4;
/// The ring: the grid's eight outer dots without its corners, clockwise
/// from the top.
const RING: [(usize, usize); 8] = [
    (1, 0),
    (2, 0),
    (3, 1),
    (3, 2),
    (2, 3),
    (1, 3),
    (0, 2),
    (0, 1),
];

/// Thinking: dots on the sphere.
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

/// Connecting: one ring dot lights per step, then they go out in turn.
const RING_STEP_MS: u64 = 100;
/// A command or tool: the trail's head moves one ring dot per step.
const ORBIT_STEP_MS: u64 = 110;
/// Writing: each row's dots move one column per step (rows differ a
/// little, like text flowing past).
const STREAM_STEP_MS: [u64; GRID] = [100, 130, 100, 130];
/// Writing: what passes along each row, eight columns that repeat.
const STREAM: [[bool; 8]; GRID] = [
    [true, false, false, true, false, false, true, false],
    [false, true, false, false, false, true, true, false],
    [true, false, true, false, false, false, false, true],
    [false, false, true, false, true, false, false, true],
];
/// Reading: the scan line moves down one row per step.
const SCAN_STEP_MS: u64 = 160;
/// Editing: one dot is typed per step, in reading order, where the text
/// has a dot; it holds, then the rows clear from the top.
const TYPING_STEP_MS: u64 = 80;
const TYPED: [[bool; GRID]; GRID] = [
    [true, true, false, true],
    [true, true, true, false],
    [true, false, true, true],
    [true, true, false, false],
];
const TYPING_HOLD_STEPS: u64 = 5;
/// Waiting: one slow breath.
const BREATH_MS: f32 = 2_600.0;
/// Compacting: the dots draw in to the centre and spread again.
const CONTRACT_MS: f32 = 1_800.0;
/// The order dots switch to a new motion as it dissolves in (scattered).
const DISSOLVE: [u8; GRID * GRID] = [7, 12, 2, 9, 14, 0, 5, 11, 3, 15, 8, 1, 10, 4, 13, 6];

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

/// What the glyph shows: what the turn is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Motion {
    /// Connecting or reconnecting: the ring lights dot by dot.
    Ring,
    /// Connected, waiting for the first token or reasoning: a turning
    /// sphere of dots.
    Sphere,
    /// Writing the reply: dots stream left to right.
    Stream,
    /// A command or another tool: a short trail orbits the ring.
    Orbit,
    /// Reading, listing, searching: a scan line moves down.
    Scan,
    /// Editing or writing a file: rows of dots typed, then cleared.
    Typing,
    /// Waiting for the user's approval, or a stalled reply: still dots,
    /// breathing.
    Breathe,
    /// Compacting the context: the dots draw in and spread again.
    Contract,
}

/// What a tool call does, as the glyph shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ToolKind {
    /// A command, or any other tool.
    #[default]
    Run,
    /// Reads, lists or searches (files or the web).
    Read,
    /// Edits or writes files.
    Edit,
}

/// A tool by its name (the engine's own, or an MCP tool's last part).
pub(crate) fn tool_kind(name: &str) -> ToolKind {
    let name = name.to_ascii_lowercase();
    let name = name.rsplit("__").next().unwrap_or_default();
    match name {
        "read" | "read_file" | "view" | "ls" | "list" | "list_dir" | "list_files" | "glob"
        | "grep" | "rg" | "ripgrep" | "search" | "find" | "outline" | "web_search"
        | "websearch" | "web_fetch" | "webfetch" | "fetch" => ToolKind::Read,
        "edit" | "multiedit" | "multi_edit" | "str_replace" | "hashline_edit" | "apply_patch"
        | "write" | "write_file" | "create_file" | "notebookedit" | "notebook_edit" => {
            ToolKind::Edit
        }
        _ => ToolKind::Run,
    }
}

/// The motion on show at a moment: the motion, how long it has shown (its
/// own animation clock), and while it dissolves in, the one it replaces
/// with that one's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MotionView {
    pub(crate) motion: Motion,
    pub(crate) for_ms: u64,
    pub(crate) before: Option<(Motion, u64)>,
}

impl MotionView {
    pub(crate) fn steady(motion: Motion, for_ms: u64) -> Self {
        Self {
            motion,
            for_ms,
            before: None,
        }
    }
}

/// Which motion shows over a turn. A motion asked for shows once it has
/// been asked for `DEBOUNCE` without a change of mind; asking for the one
/// on show again drops a change still waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MotionTrack {
    shown: Motion,
    shown_at: Instant,
    before: Option<(Motion, Instant)>,
    pending: Option<(Motion, Instant)>,
}

impl MotionTrack {
    /// The turn's first motion shows at once.
    pub(crate) fn new(motion: Motion, now: Instant) -> Self {
        Self {
            shown: motion,
            shown_at: now,
            before: None,
            pending: None,
        }
    }

    pub(crate) fn request(&mut self, motion: Motion, now: Instant) {
        self.settle(now);
        if motion == self.shown {
            self.pending = None;
        } else if self.pending.map(|(pending, _)| pending) != Some(motion) {
            self.pending = Some((motion, now));
        }
    }

    pub(crate) fn view(&self, now: Instant) -> MotionView {
        let mut track = self.clone();
        track.settle(now);
        let for_ms = millis(now.saturating_duration_since(track.shown_at));
        let before = track
            .before
            .filter(|_| for_ms < BLEND_MS)
            .map(|(motion, at)| (motion, millis(now.saturating_duration_since(at))));
        MotionView {
            motion: track.shown,
            for_ms,
            before,
        }
    }

    /// A change asked for long enough shows, from when it became due.
    fn settle(&mut self, now: Instant) {
        if let Some((motion, asked)) = self.pending {
            let due = asked + DEBOUNCE;
            if now >= due {
                self.before = Some((self.shown, self.shown_at));
                self.shown = motion;
                self.shown_at = due;
                self.pending = None;
            }
        }
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

/// A running turn as the row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Progress {
    pub(crate) phase: Phase,
    /// Time since the turn started: the counter and the label's light.
    pub(crate) elapsed: Duration,
    pub(crate) motion: MotionView,
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
    let ms = millis(progress.elapsed);
    let waiting = progress.phase == Phase::Waiting;
    let mut spans = vec![Span::raw("  ")];
    spans.extend(glyph_spans(theme, &progress.motion, ms, waiting));
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

/// One frame of the glyph: the dots of each braille cell and how bright
/// each cell is (0 dim … 1 full).
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

/// Dots put down for one frame; a cell's brightness is the mean of its
/// dots'.
#[derive(Default)]
struct Canvas {
    cells: [u8; 2],
    shade_sum: [f32; 2],
    shade_count: [u32; 2],
}

impl Canvas {
    fn dot(&mut self, column: usize, row: usize, shade: f32) {
        let (column, row) = (column.min(GRID - 1), row.min(GRID - 1));
        let cell = column / 2;
        self.cells[cell] |= dot_bit(column % 2, row);
        self.shade_sum[cell] += shade.clamp(0.0, 1.0);
        self.shade_count[cell] += 1;
    }

    fn frame(self) -> GlyphFrame {
        let shade = [0, 1].map(|cell| {
            if self.shade_count[cell] == 0 {
                1.0
            } else {
                self.shade_sum[cell] / self.shade_count[cell] as f32
            }
        });
        GlyphFrame {
            cells: self.cells,
            shade,
        }
    }
}

/// The ring, still: the glyph of a row that does not move.
pub(crate) fn still_frame() -> GlyphFrame {
    let mut canvas = Canvas::default();
    for (column, row) in RING {
        canvas.dot(column, row, 1.0);
    }
    canvas.frame()
}

/// The glyph for the motion on show, dissolving from the one before while
/// it blends in.
pub(crate) fn glyph_frame(view: &MotionView) -> GlyphFrame {
    let frame = motion_frame(view.motion, view.for_ms);
    match view.before {
        Some((before, before_ms)) if view.for_ms < BLEND_MS => dissolve(
            &motion_frame(before, before_ms),
            &frame,
            view.for_ms as f32 / BLEND_MS as f32,
        ),
        _ => frame,
    }
}

/// `motion`, `ms` after it began showing.
pub(crate) fn motion_frame(motion: Motion, ms: u64) -> GlyphFrame {
    let mut canvas = Canvas::default();
    match motion {
        Motion::Ring => {
            // Grows to the full ring, then goes out from where it began,
            // down to one dot (never none).
            let step = (ms / RING_STEP_MS % (2 * RING.len() as u64 - 1)) as usize;
            let (first, last) = if step < RING.len() {
                (0, step)
            } else {
                (step + 1 - RING.len(), RING.len() - 1)
            };
            for (column, row) in &RING[first..=last] {
                canvas.dot(*column, *row, 1.0);
            }
        }
        Motion::Sphere => {
            for point in sphere(ms) {
                if point.depth <= HIDDEN_BELOW {
                    continue;
                }
                // 0 at the sphere's visible edge … 1 facing the viewer.
                let shade = (point.depth - HIDDEN_BELOW) / (1.0 - HIDDEN_BELOW);
                canvas.dot(grid(point.column), grid(point.row), shade);
            }
        }
        Motion::Stream => {
            for (row, tape) in STREAM.iter().enumerate() {
                let shift = (ms / STREAM_STEP_MS[row]) as usize;
                for column in 0..GRID {
                    let at = (column + tape.len() - shift % tape.len()) % tape.len();
                    if tape[at] {
                        canvas.dot(column, row, 0.9);
                    }
                }
            }
        }
        Motion::Orbit => {
            let head = (ms / ORBIT_STEP_MS) as usize;
            for (behind, shade) in [1.0, 0.7, 0.45].into_iter().enumerate() {
                let (column, row) = RING[(head + 2 * RING.len() - behind) % RING.len()];
                canvas.dot(column, row, shade);
            }
        }
        Motion::Scan => {
            let row = (ms / SCAN_STEP_MS) as usize % GRID;
            for column in 0..GRID {
                canvas.dot(column, row, 1.0);
            }
        }
        Motion::Typing => {
            let typed: Vec<(usize, usize)> = (0..GRID)
                .flat_map(|row| (0..GRID).map(move |column| (column, row)))
                .filter(|(column, row)| TYPED[*row][*column])
                .collect();
            let count = typed.len() as u64;
            // Typed one by one, held, then cleared a row at a time.
            let cycle = count + TYPING_HOLD_STEPS + GRID as u64 - 1;
            let step = ms / TYPING_STEP_MS % cycle;
            let (shown, first_row) = if step < count {
                (step as usize + 1, 0)
            } else if step < count + TYPING_HOLD_STEPS {
                (typed.len(), 0)
            } else {
                (typed.len(), (step - count - TYPING_HOLD_STEPS + 1) as usize)
            };
            for (column, row) in &typed[..shown] {
                if *row >= first_row {
                    canvas.dot(*column, *row, 1.0);
                }
            }
        }
        Motion::Breathe => {
            let phase = std::f32::consts::TAU * (ms as f64 % BREATH_MS as f64) as f32 / BREATH_MS;
            let shade = 0.3 + 0.7 * (0.5 - 0.5 * phase.cos());
            for (column, row) in RING {
                canvas.dot(column, row, shade);
            }
        }
        Motion::Contract => {
            use std::f32::consts::{PI, TAU};
            let phase = TAU * (ms as f64 % CONTRACT_MS as f64) as f32 / CONTRACT_MS;
            // 1.5 is the ring; 0.55 the four centre dots.
            let radius = 0.55 + 0.95 * (0.5 + 0.5 * phase.cos());
            for index in 0..RING.len() {
                let angle = PI / 8.0 + index as f32 * PI / 4.0;
                let (sin, cos) = angle.sin_cos();
                canvas.dot(grid(1.5 + radius * cos), grid(1.5 - radius * sin), 1.0);
            }
        }
    }
    canvas.frame()
}

/// A fractional grid position, rounded onto the grid.
fn grid(value: f32) -> usize {
    value.round().clamp(0.0, (GRID - 1) as f32) as usize
}

/// `from` turning into `to`: dots switch over in a scattered order as
/// `amount` goes from 0 to 1; brightness blends.
fn dissolve(from: &GlyphFrame, to: &GlyphFrame, amount: f32) -> GlyphFrame {
    let amount = amount.clamp(0.0, 1.0);
    let mut cells = [0u8; 2];
    for row in 0..GRID {
        for column in 0..GRID {
            let source = if (DISSOLVE[row * GRID + column] as f32) < amount * 16.0 {
                to
            } else {
                from
            };
            let (cell, bit) = (column / 2, dot_bit(column % 2, row));
            cells[cell] |= source.cells[cell] & bit;
        }
    }
    let shade = [0, 1].map(|cell| from.shade[cell] + (to.shade[cell] - from.shade[cell]) * amount);
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

/// The sphere's dots `ms` into its motion: spread evenly (a Fibonacci
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
fn glyph_spans(
    theme: &Theme,
    view: &MotionView,
    turn_ms: u64,
    waiting: bool,
) -> Vec<Span<'static>> {
    if !theme.rich_glyphs {
        let frame = if theme.animate {
            ASCII_FRAMES[(turn_ms / ASCII_FRAME_MS % ASCII_FRAMES.len() as u64) as usize]
        } else {
            "*"
        };
        return vec![Span::styled(frame, glyph_style(theme, 1.0, waiting))];
    }
    let frame = if theme.animate {
        glyph_frame(view)
    } else {
        still_frame()
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

/// The glyph's colour: the accent (attention while the reply stalls),
/// stepped toward the background for dimmer dots where the background is
/// known.
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
    use std::collections::HashSet;
    use unicode_width::UnicodeWidthStr;

    const DARK: Theme = Theme::new(ColorDepth::TrueColor, Some((22, 21, 19)));
    const ALL: [Motion; 8] = [
        Motion::Ring,
        Motion::Sphere,
        Motion::Stream,
        Motion::Orbit,
        Motion::Scan,
        Motion::Typing,
        Motion::Breathe,
        Motion::Contract,
    ];

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

    fn progress(phase: Phase, ms: u64, motion: Motion) -> Progress {
        Progress {
            phase,
            elapsed: Duration::from_millis(ms),
            motion: MotionView::steady(motion, ms),
        }
    }

    fn dots(motion: Motion, ms: u64) -> HashSet<(usize, usize)> {
        motion_frame(motion, ms).dots().into_iter().collect()
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
        for motion in ALL {
            let row = line(&DARK, &progress(Phase::Working, 41_000, motion));
            let text = text_of(&row);
            assert!(text.ends_with(" 工作中 41 秒"), "{text}");
            // Padding, two glyph cells, one space, then the label.
            let before = &text[..text.find("工作中").unwrap()];
            assert_eq!(UnicodeWidthStr::width(before), 5, "{motion:?}");
        }
        with_lang(Lang::En, || {
            let row = line(&DARK, &progress(Phase::Working, 65_000, Motion::Stream));
            assert!(text_of(&row).ends_with(" Working 1m 05s"));
        });
    }

    #[test]
    fn every_motion_is_two_cells_deterministic_and_never_blank() {
        for motion in ALL {
            for ms in (0..12_000).step_by(83) {
                let frame = motion_frame(motion, ms);
                assert_eq!(frame, motion_frame(motion, ms));
                assert_eq!(UnicodeWidthStr::width(frame.text().as_str()), 2);
                assert!(!frame.dots().is_empty(), "{motion:?} blank at {ms} ms");
                assert!(frame.shade.iter().all(|shade| (0.0..=1.0).contains(shade)));
            }
        }
    }

    #[test]
    fn each_motion_looks_its_own() {
        let runs: Vec<Vec<[u8; 2]>> = ALL
            .iter()
            .map(|motion| {
                (0..3_000)
                    .step_by(83)
                    .map(|ms| motion_frame(*motion, ms).cells)
                    .collect()
            })
            .collect();
        for (a, first) in runs.iter().enumerate() {
            for second in &runs[a + 1..] {
                assert_ne!(first, second);
            }
        }
    }

    #[test]
    fn sphere_keeps_a_handful_of_dots_and_turns_gently() {
        let mut changed = 0;
        let steps = 9_000 / 83;
        for step in 1..steps {
            let (before, after) = (
                dots(Motion::Sphere, (step - 1) * 83),
                dots(Motion::Sphere, step * 83),
            );
            assert!((3..=10).contains(&after.len()), "{} dots", after.len());
            changed += before.symmetric_difference(&after).count();
        }
        // About two dots change a frame: it turns, it does not flicker.
        let mean = changed as f32 / (steps - 1) as f32;
        assert!(mean > 0.5 && mean < 3.5, "{mean} dots change per frame");
    }

    #[test]
    fn stream_flows_left_to_right() {
        for (row, step) in STREAM_STEP_MS.iter().enumerate() {
            for start in [0, 7 * step] {
                let (now, next) = (
                    dots(Motion::Stream, start),
                    dots(Motion::Stream, start + step),
                );
                for column in 0..GRID - 1 {
                    assert_eq!(
                        now.contains(&(column, row)),
                        next.contains(&(column + 1, row)),
                        "row {row} column {column}"
                    );
                }
            }
        }
    }

    #[test]
    fn orbit_runs_a_three_dot_trail_round_the_ring() {
        for step in 0..20 {
            let lit = dots(Motion::Orbit, step * ORBIT_STEP_MS);
            assert_eq!(lit.len(), 3);
            assert!(lit.iter().all(|dot| RING.contains(dot)));
            assert!(lit.contains(&RING[step as usize % RING.len()]));
        }
    }

    #[test]
    fn scan_line_moves_down_a_row_at_a_time() {
        for step in 0..8u64 {
            let row = step as usize % GRID;
            let expected: HashSet<_> = (0..GRID).map(|column| (column, row)).collect();
            assert_eq!(dots(Motion::Scan, step * SCAN_STEP_MS), expected);
        }
    }

    #[test]
    fn typing_fills_in_reading_order_holds_then_clears() {
        let counts: Vec<usize> = (0..19)
            .map(|step| dots(Motion::Typing, step * TYPING_STEP_MS).len())
            .collect();
        assert_eq!(
            counts,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 11, 11, 11, 11, 11, 8, 5, 2]
        );
        // The first dots are the top row's, left to right.
        assert_eq!(dots(Motion::Typing, 0), HashSet::from([(0, 0)]));
        assert_eq!(
            dots(Motion::Typing, TYPING_STEP_MS),
            HashSet::from([(0, 0), (1, 0)])
        );
        // Then it starts again.
        assert_eq!(dots(Motion::Typing, 19 * TYPING_STEP_MS).len(), 1);
    }

    #[test]
    fn ring_lights_one_by_one_then_goes_out() {
        let counts: Vec<usize> = (0..16)
            .map(|step| dots(Motion::Ring, step * RING_STEP_MS).len())
            .collect();
        assert_eq!(counts, [1, 2, 3, 4, 5, 6, 7, 8, 7, 6, 5, 4, 3, 2, 1, 1]);
    }

    #[test]
    fn breathing_keeps_still_dots_and_changes_brightness() {
        let shades: Vec<f32> = (0..2_600)
            .step_by(83)
            .map(|ms| {
                assert_eq!(dots(Motion::Breathe, ms), dots(Motion::Breathe, 0));
                motion_frame(Motion::Breathe, ms).shade[0]
            })
            .collect();
        let low = shades.iter().copied().fold(1.0, f32::min);
        let high = shades.iter().copied().fold(0.0, f32::max);
        assert!(low < 0.4 && high > 0.9, "{low} … {high}");
    }

    #[test]
    fn contract_draws_to_the_centre_and_spreads_again() {
        let ring: HashSet<_> = RING.into_iter().collect();
        assert_eq!(dots(Motion::Contract, 0), ring);
        let centre = HashSet::from([(1, 1), (2, 1), (1, 2), (2, 2)]);
        assert_eq!(dots(Motion::Contract, CONTRACT_MS as u64 / 2), centre);
        assert_eq!(dots(Motion::Contract, CONTRACT_MS as u64), ring);
    }

    #[test]
    fn a_new_motion_shows_once_asked_for_long_enough() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut track = MotionTrack::new(Motion::Sphere, start);
        assert_eq!(track.view(at(500)), MotionView::steady(Motion::Sphere, 500));

        track.request(Motion::Orbit, at(1_000));
        assert_eq!(track.view(at(1_399)).motion, Motion::Sphere);
        let view = track.view(at(1_400));
        assert_eq!(view.motion, Motion::Orbit);
        assert_eq!(view.for_ms, 0);
        assert_eq!(view.before, Some((Motion::Sphere, 1_400)));

        // A flip back within the debounce never shows.
        track.request(Motion::Stream, at(2_000));
        track.request(Motion::Orbit, at(2_300));
        assert_eq!(
            track.view(at(3_000)),
            MotionView::steady(Motion::Orbit, 1_600)
        );

        // A change of mind starts the wait again.
        track.request(Motion::Scan, at(4_000));
        track.request(Motion::Typing, at(4_200));
        assert_eq!(track.view(at(4_500)).motion, Motion::Orbit);
        assert_eq!(track.view(at(4_600)).motion, Motion::Typing);
        // Asking again for the motion waiting does not restart its wait.
        track.request(Motion::Scan, at(5_000));
        track.request(Motion::Scan, at(5_300));
        assert_eq!(track.view(at(5_400)).motion, Motion::Scan);
    }

    #[test]
    fn a_new_motion_dissolves_in() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut track = MotionTrack::new(Motion::Sphere, start);
        track.request(Motion::Scan, at(0));
        let halfway = track.view(at(400 + BLEND_MS / 2));
        assert_eq!(halfway.before, Some((Motion::Sphere, 400 + BLEND_MS / 2)));
        let mixed: HashSet<_> = glyph_frame(&halfway).dots().into_iter().collect();
        let old = dots(Motion::Sphere, 400 + BLEND_MS / 2);
        let new = dots(Motion::Scan, BLEND_MS / 2);
        assert!(mixed.is_subset(&old.union(&new).copied().collect()));
        // Half the dots have switched: on the scan line's row, (0, 0) and
        // (2, 0) come from the scan, (1, 0) and (3, 0) still from the sphere.
        assert!(mixed.contains(&(0, 0)) && mixed.contains(&(2, 0)));
        assert_eq!(mixed.contains(&(1, 0)), old.contains(&(1, 0)));
        assert_eq!(mixed.contains(&(3, 0)), old.contains(&(3, 0)));

        let done = track.view(at(400 + BLEND_MS));
        assert_eq!(done.before, None);
        assert_eq!(glyph_frame(&done), motion_frame(Motion::Scan, BLEND_MS));

        let (from, to) = (motion_frame(Motion::Ring, 0), motion_frame(Motion::Scan, 0));
        assert_eq!(dissolve(&from, &to, 0.0).cells, from.cells);
        assert_eq!(dissolve(&from, &to, 1.0).cells, to.cells);
    }

    #[test]
    fn tools_read_as_run_read_or_edit() {
        for name in [
            "read",
            "ls",
            "ripgrep",
            "outline",
            "web_search",
            "web_fetch",
            "Grep",
            "Glob",
            "WebFetch",
            "mcp__files__read_file",
        ] {
            assert_eq!(tool_kind(name), ToolKind::Read, "{name}");
        }
        for name in [
            "edit",
            "str_replace",
            "hashline_edit",
            "apply_patch",
            "write",
            "MultiEdit",
            "NotebookEdit",
        ] {
            assert_eq!(tool_kind(name), ToolKind::Edit, "{name}");
        }
        for name in [
            "bash",
            "exec_command",
            "write_stdin",
            "spawn_agent",
            "generate_image",
            "mcp__deploy__run",
        ] {
            assert_eq!(tool_kind(name), ToolKind::Run, "{name}");
        }
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
        let rest = sweep_style(&DARK, 0.0).fg;
        let lit = sweep_style(&DARK, 1.0).fg;
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
    fn no_colour_keeps_the_dots_moving_without_colour() {
        let none = Theme::new(ColorDepth::None, None);
        for motion in ALL {
            let first = line(&none, &progress(Phase::Working, 0, motion));
            assert!(first
                .spans
                .iter()
                .all(|span| matches!(span.style.fg, None | Some(Color::Reset))));
            assert!(text_of(&first).ends_with(" 工作中 0 秒"));
            if motion == Motion::Breathe {
                continue;
            }
            let glyphs: HashSet<String> = (0..40)
                .map(|step| {
                    let row = line(&none, &progress(Phase::Working, step * 83, motion));
                    text_of(&row)[2..].chars().take(2).collect()
                })
                .collect();
            assert!(glyphs.len() > 1, "{motion:?} moves");
        }
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
                &progress(Phase::Working, step * 83, Motion::Orbit),
            );
            let text = text_of(&row);
            assert!(ASCII_FRAMES.contains(&&text[2..3]), "{text}");
            assert!(text[..text.find("工作中").unwrap()].is_ascii());
        }
    }

    #[test]
    fn still_row_shows_the_ring_and_plain_label() {
        let still = Theme {
            animate: false,
            ..DARK
        };
        let row = |ms, motion| line(&still, &progress(Phase::Working, ms, motion));
        assert_eq!(text_of(&row(0, Motion::Stream)), "  ⢎⡱ 工作中 0 秒");
        // Nothing but the seconds changes, whatever the turn does.
        assert_eq!(row(300, Motion::Orbit).spans, row(0, Motion::Stream).spans);
        assert_eq!(text_of(&row(5_000, Motion::Scan)), "  ⢎⡱ 工作中 5 秒");
        assert_eq!(redraw_interval(&still), STILL_INTERVAL);
        assert_eq!(redraw_interval(&DARK), FRAME_INTERVAL);
    }

    #[test]
    fn waiting_row_uses_the_attention_tone() {
        let row = line(&DARK, &progress(Phase::Waiting, 20_000, Motion::Breathe));
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
