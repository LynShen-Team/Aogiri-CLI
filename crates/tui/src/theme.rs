//! Semantic colours, resolved once for the terminal at hand.
//!
//! Text keeps the terminal's own foreground; only a few roles carry colour
//! (accent, additions, deletions, attention). Each role resolves through the
//! colour depth (`NO_COLOR` → none, 16 named colours, the 256 palette, or
//! truecolor) and the background (queried from the terminal, or `COLORFGBG`):
//! dark and light tones where the background is known, a mid tone that reads
//! on both where it is not. Background fills (user turns, diff lines,
//! selection) are blended from the detected background and only drawn when
//! it is known and the terminal has at least 256 colours.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

pub(crate) type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorDepth {
    /// `NO_COLOR` or a dumb terminal: bold, dim and reverse only.
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Theme {
    pub(crate) depth: ColorDepth,
    /// The terminal's background colour, when known.
    pub(crate) background: Option<Rgb>,
    /// The terminal draws block elements and braille (false on the legacy
    /// Windows console, whose fonts lack them).
    pub(crate) rich_glyphs: bool,
    /// The working indicator may move (`working_indicator::motion_allowed`).
    pub(crate) animate: bool,
}

/// One role's colour on a dark and a light background, a mid tone for an
/// unknown one, and the named colour a 16-colour terminal uses.
struct Tone {
    dark: Rgb,
    light: Rgb,
    mid: Rgb,
    ansi: Color,
}

const ACCENT: Tone = Tone {
    dark: (47, 180, 203),
    light: (14, 127, 148),
    mid: (30, 156, 179),
    ansi: Color::Cyan,
};
const DIM: Tone = Tone {
    dark: (142, 136, 126),
    light: (111, 106, 99),
    mid: (135, 130, 123),
    ansi: Color::DarkGray,
};
const FAINT: Tone = Tone {
    dark: (78, 75, 70),
    light: (201, 196, 187),
    mid: (110, 106, 100),
    ansi: Color::DarkGray,
};
const ADD: Tone = Tone {
    dark: (124, 196, 143),
    light: (34, 112, 63),
    mid: (79, 163, 106),
    ansi: Color::Green,
};
const DEL: Tone = Tone {
    dark: (224, 124, 106),
    light: (178, 58, 38),
    mid: (208, 96, 76),
    ansi: Color::Red,
};
const WARN: Tone = Tone {
    dark: (226, 180, 92),
    light: (156, 90, 12),
    mid: (201, 138, 46),
    ansi: Color::Yellow,
};
const INFO: Tone = Tone {
    dark: (111, 184, 214),
    light: (31, 108, 140),
    mid: (74, 149, 184),
    ansi: Color::Blue,
};
const CODE: Tone = Tone {
    dark: (217, 180, 138),
    light: (138, 75, 28),
    mid: (184, 131, 79),
    ansi: Color::Yellow,
};

/// Background fills, blended from the terminal's background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// A user turn in the transcript.
    User,
    /// The highlighted row of a list.
    Selected,
    AddLine,
    DelLine,
    AddWord,
    DelWord,
    /// A mouse drag selection.
    Drag,
}

const DARK_FALLBACK: Rgb = (22, 21, 19);

impl Theme {
    pub(crate) const fn new(depth: ColorDepth, background: Option<Rgb>) -> Self {
        Self {
            depth,
            background,
            rich_glyphs: true,
            animate: true,
        }
    }

    pub(crate) fn is_dark(&self) -> Option<bool> {
        self.background.map(|bg| luminance(bg) < 128.0)
    }

    fn resolve(&self, tone: &Tone) -> Option<Color> {
        self.color(self.tone_rgb(tone), tone.ansi)
    }

    fn tone_rgb(&self, tone: &Tone) -> Rgb {
        match self.is_dark() {
            Some(true) => tone.dark,
            Some(false) => tone.light,
            None => tone.mid,
        }
    }

    /// The accent and attention tones for this background, for blends.
    pub(crate) fn accent_rgb(&self) -> Rgb {
        self.tone_rgb(&ACCENT)
    }
    pub(crate) fn warn_rgb(&self) -> Rgb {
        self.tone_rgb(&WARN)
    }

    /// `rgb` at this colour depth (`ansi` on 16 colours, none without colour).
    pub(crate) fn color(&self, rgb: Rgb, ansi: Color) -> Option<Color> {
        match self.depth {
            ColorDepth::None => None,
            ColorDepth::Ansi16 => Some(ansi),
            ColorDepth::Ansi256 => Some(Color::Indexed(nearest_256(rgb))),
            ColorDepth::TrueColor => Some(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        }
    }

    fn fg(&self, tone: &Tone, fallback: Modifier) -> Style {
        match self.resolve(tone) {
            Some(color) => Style::new().fg(color),
            None => Style::new().add_modifier(fallback),
        }
    }

    pub(crate) fn accent(&self) -> Style {
        self.fg(&ACCENT, Modifier::BOLD)
    }
    pub(crate) fn dim(&self) -> Style {
        self.fg(&DIM, Modifier::DIM)
    }
    pub(crate) fn faint(&self) -> Style {
        self.fg(&FAINT, Modifier::DIM)
    }
    pub(crate) fn add(&self) -> Style {
        self.fg(&ADD, Modifier::empty())
    }
    pub(crate) fn del(&self) -> Style {
        self.fg(&DEL, Modifier::empty())
    }
    pub(crate) fn warn(&self) -> Style {
        self.fg(&WARN, Modifier::BOLD)
    }
    pub(crate) fn err(&self) -> Style {
        self.fg(&DEL, Modifier::BOLD)
    }
    pub(crate) fn info(&self) -> Style {
        self.fg(&INFO, Modifier::empty())
    }
    pub(crate) fn code(&self) -> Style {
        self.fg(&CODE, Modifier::empty())
    }

    /// A background fill, or None where fills are not drawn (no colour,
    /// 16 colours, or an unknown background).
    pub(crate) fn surface(&self, surface: Surface) -> Option<Color> {
        if matches!(self.depth, ColorDepth::None | ColorDepth::Ansi16) {
            return None;
        }
        let bg = self.background?;
        let dark = luminance(bg) < 128.0;
        let (toward, amount) = match (surface, dark) {
            (Surface::User, true) => ((255, 255, 255), 0.07),
            (Surface::User, false) => ((0, 0, 0), 0.05),
            (Surface::Selected, true) => ((255, 255, 255), 0.10),
            (Surface::Selected, false) => ((0, 0, 0), 0.07),
            (Surface::AddLine, true) => ((63, 164, 91), 0.16),
            (Surface::AddLine, false) => ((63, 164, 91), 0.13),
            (Surface::DelLine, true) => ((217, 85, 62), 0.16),
            (Surface::DelLine, false) => ((217, 85, 62), 0.12),
            (Surface::AddWord, true) => ((63, 164, 91), 0.34),
            (Surface::AddWord, false) => ((63, 164, 91), 0.28),
            (Surface::DelWord, true) => ((217, 85, 62), 0.34),
            (Surface::DelWord, false) => ((217, 85, 62), 0.26),
            (Surface::Drag, true) => (ACCENT.dark, 0.30),
            (Surface::Drag, false) => (ACCENT.light, 0.22),
        };
        let rgb = mix(bg, toward, amount);
        self.color(rgb, Color::Reset)
    }
}

/// The theme the TUI draws with: detected at startup, a dark truecolor
/// default before that (and in tests).
pub(crate) fn current() -> Theme {
    #[cfg(any(test, feature = "preview"))]
    if let Some(theme) = TEST_THEME.with(|cell| cell.get()) {
        return theme;
    }
    THEME
        .get()
        .copied()
        .unwrap_or(Theme::new(ColorDepth::TrueColor, Some(DARK_FALLBACK)))
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Fixes the theme for this process (first call wins).
pub(crate) fn install(theme: Theme) {
    let _ = THEME.set(theme);
}

#[cfg(any(test, feature = "preview"))]
thread_local! {
    static TEST_THEME: std::cell::Cell<Option<Theme>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with `theme` as the current theme on this thread.
#[cfg(any(test, feature = "preview"))]
pub(crate) fn with_theme<T>(theme: Theme, f: impl FnOnce() -> T) -> T {
    TEST_THEME.with(|cell| cell.set(Some(theme)));
    let result = f();
    TEST_THEME.with(|cell| cell.set(None));
    result
}

pub(crate) fn accent() -> Style {
    current().accent()
}
pub(crate) fn dim() -> Style {
    current().dim()
}
pub(crate) fn faint() -> Style {
    current().faint()
}
pub(crate) fn add() -> Style {
    current().add()
}
pub(crate) fn del() -> Style {
    current().del()
}
pub(crate) fn warn() -> Style {
    current().warn()
}
pub(crate) fn err() -> Style {
    current().err()
}
pub(crate) fn info() -> Style {
    current().info()
}
pub(crate) fn code() -> Style {
    current().code()
}
pub(crate) fn surface(surface: Surface) -> Option<Color> {
    current().surface(surface)
}
pub(crate) fn rich_glyphs() -> bool {
    current().rich_glyphs
}

/// Colour depth from the environment, the way most terminal tools read it.
pub(crate) fn detect_depth(var: impl Fn(&str) -> Option<String>) -> ColorDepth {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    if set("NO_COLOR").is_some() {
        return ColorDepth::None;
    }
    let term = set("TERM").unwrap_or_default().to_ascii_lowercase();
    if term == "dumb" {
        return ColorDepth::None;
    }
    let colorterm = set("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorDepth::TrueColor;
    }
    let program = set("TERM_PROGRAM").unwrap_or_default();
    if set("WT_SESSION").is_some()
        || matches!(
            program.as_str(),
            "iTerm.app" | "WezTerm" | "vscode" | "ghostty" | "Hyper" | "Tabby" | "rio"
        )
        || term.contains("truecolor")
        || term.contains("direct")
        || term == "xterm-kitty"
        || term == "xterm-ghostty"
    {
        return ColorDepth::TrueColor;
    }
    if term.contains("256") || program == "Apple_Terminal" {
        return ColorDepth::Ansi256;
    }
    if cfg!(windows) {
        // The Windows 10+ console takes 24-bit colour through VT sequences.
        return ColorDepth::TrueColor;
    }
    ColorDepth::Ansi16
}

/// `LYNSHEN_THEME=dark|light` forces the background; otherwise `COLORFGBG`
/// ("15;0" style: foreground;background palette slots) when set.
pub(crate) fn background_from_env(var: impl Fn(&str) -> Option<String>) -> Option<Rgb> {
    match var("LYNSHEN_THEME").as_deref().map(str::trim) {
        Some("dark") => return Some(DARK_FALLBACK),
        Some("light") => return Some((251, 250, 247)),
        _ => {}
    }
    let value = var("COLORFGBG")?;
    let slot: u8 = value.rsplit(';').next()?.trim().parse().ok()?;
    // Slots 0-6 and 8 are dark grounds; 7 and 9-15 light.
    Some(if slot == 7 || slot >= 9 {
        (251, 250, 247)
    } else {
        DARK_FALLBACK
    })
}

/// Detects the theme. Call in raw mode, before input is read: the
/// background query reads the terminal's reply from stdin.
pub(crate) fn detect() -> Theme {
    let var = |name: &str| std::env::var(name).ok();
    let depth = detect_depth(var);
    let forced = matches!(
        var("LYNSHEN_THEME").as_deref().map(str::trim),
        Some("dark" | "light")
    );
    let background = if forced || depth == ColorDepth::None {
        background_from_env(var)
    } else {
        query_background().or_else(|| background_from_env(var))
    };
    let rich_glyphs =
        !cfg!(windows) || var("WT_SESSION").is_some() || var("TERM_PROGRAM").is_some();
    let animate = crate::working_indicator::motion_allowed(var, {
        use std::io::IsTerminal;
        std::io::stdout().is_terminal()
    });
    Theme {
        depth,
        background,
        rich_glyphs,
        animate,
    }
}

/// Asks the terminal for its background colour (OSC 11), followed by a
/// device-attributes request every terminal answers: reading stops at that
/// answer, so a terminal that ignores OSC 11 costs one round trip and
/// leaves nothing behind for the key reader.
#[cfg(unix)]
fn query_background() -> Option<Rgb> {
    use std::io::Write;
    use std::time::{Duration, Instant};

    // SAFETY: isatty only inspects the descriptors.
    if unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } {
        return None;
    }
    let mut stdout = std::io::stdout();
    stdout.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    stdout.flush().ok()?;

    let deadline = Instant::now() + Duration::from_millis(300);
    let mut reply = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut fds, 1, left.as_millis() as libc::c_int) };
        if ready <= 0 {
            break;
        }
        // SAFETY: reads into a local buffer of the given length.
        let read = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if read <= 0 {
            break;
        }
        reply.extend_from_slice(&buf[..read as usize]);
        if device_attributes_done(&reply) {
            break;
        }
    }
    parse_osc11(&reply)
}

#[cfg(not(unix))]
fn query_background() -> Option<Rgb> {
    None
}

/// The DA1 reply (`ESC [ ? … c`) has arrived.
#[cfg_attr(not(unix), allow(dead_code))]
fn device_attributes_done(reply: &[u8]) -> bool {
    let Some(start) = reply.windows(3).position(|w| w == b"\x1b[?") else {
        return false;
    };
    reply[start + 3..].contains(&b'c')
}

/// Parses `ESC ] 11 ; rgb:RRRR/GGGG/BBBB` (1–4 hex digits per channel).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn parse_osc11(reply: &[u8]) -> Option<Rgb> {
    let text = String::from_utf8_lossy(reply);
    let start = text.find("]11;")? + 4;
    let rest = &text[start..];
    let rest = rest
        .strip_prefix("rgb:")
        .or_else(|| rest.strip_prefix("rgba:"))?;
    let end = rest
        .find(|ch: char| !(ch.is_ascii_hexdigit() || ch == '/'))
        .unwrap_or(rest.len());
    let mut channels = rest[..end].split('/').map(|hex| {
        if hex.is_empty() || hex.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(hex, 16).ok()?;
        let max = (1u32 << (4 * hex.len() as u32)) - 1;
        Some((value * 255 / max) as u8)
    });
    Some((channels.next()??, channels.next()??, channels.next()??))
}

fn luminance(rgb: Rgb) -> f32 {
    0.299 * rgb.0 as f32 + 0.587 * rgb.1 as f32 + 0.114 * rgb.2 as f32
}

pub(crate) fn mix(from: Rgb, to: Rgb, amount: f32) -> Rgb {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    (
        channel(from.0, to.0),
        channel(from.1, to.1),
        channel(from.2, to.2),
    )
}

/// The nearest xterm-256 entry: the 6×6×6 cube or the grey ramp.
pub(crate) fn nearest_256(rgb: Rgb) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let cube_index = |value: u8| {
        STEPS
            .iter()
            .enumerate()
            .min_by_key(|(_, step)| (**step as i32 - value as i32).abs())
            .map(|(index, _)| index as u8)
            .unwrap_or(0)
    };
    let (r, g, b) = (cube_index(rgb.0), cube_index(rgb.1), cube_index(rgb.2));
    let cube = (STEPS[r as usize], STEPS[g as usize], STEPS[b as usize]);
    let average = (rgb.0 as u32 + rgb.1 as u32 + rgb.2 as u32) / 3;
    let grey_index = ((average.saturating_sub(8)) / 10).min(23) as u8;
    let grey_level = 8 + 10 * grey_index;
    let grey = (grey_level, grey_level, grey_level);
    let distance = |a: Rgb, b: Rgb| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
    };
    if distance(rgb, grey) < distance(rgb, cube) {
        232 + grey_index
    } else {
        16 + 36 * r + 6 * g + b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn no_color_turns_colour_off() {
        assert_eq!(
            detect_depth(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")])),
            ColorDepth::None
        );
        // An empty NO_COLOR does not count.
        assert_ne!(
            detect_depth(env(&[("NO_COLOR", ""), ("COLORTERM", "truecolor")])),
            ColorDepth::None
        );
    }

    #[test]
    fn colour_depth_follows_colorterm_and_term() {
        assert_eq!(
            detect_depth(env(&[("COLORTERM", "24bit")])),
            ColorDepth::TrueColor
        );
        assert_eq!(
            detect_depth(env(&[("TERM", "xterm-256color")])),
            ColorDepth::Ansi256
        );
        assert_eq!(detect_depth(env(&[("TERM", "dumb")])), ColorDepth::None);
        if !cfg!(windows) {
            assert_eq!(detect_depth(env(&[("TERM", "xterm")])), ColorDepth::Ansi16);
        }
    }

    #[test]
    fn osc11_reply_parses_any_hex_width() {
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:ffff/fafa/f7f7\x1b\\"),
            Some((255, 250, 247))
        );
        assert_eq!(parse_osc11(b"\x1b]11;rgb:16/15/13\x07"), Some((22, 21, 19)));
        assert_eq!(parse_osc11(b"\x1b[?62;22c"), None);
        assert!(device_attributes_done(b"\x1b]11;rgb:0/0/0\x07\x1b[?1;2c"));
        assert!(!device_attributes_done(b"\x1b]11;rgb:0/0/0\x07"));
    }

    #[test]
    fn colorfgbg_and_override_set_the_background() {
        let dark = background_from_env(env(&[("COLORFGBG", "15;0")])).unwrap();
        assert!(luminance(dark) < 128.0);
        let light = background_from_env(env(&[("COLORFGBG", "0;15")])).unwrap();
        assert!(luminance(light) > 128.0);
        let forced = background_from_env(env(&[("LYNSHEN_THEME", "light"), ("COLORFGBG", "15;0")]));
        assert!(luminance(forced.unwrap()) > 128.0);
    }

    #[test]
    fn fills_need_a_known_background_and_256_colours() {
        let known = Theme::new(ColorDepth::TrueColor, Some((255, 255, 255)));
        assert!(known.surface(Surface::AddLine).is_some());
        let unknown = Theme::new(ColorDepth::TrueColor, None);
        assert_eq!(unknown.surface(Surface::AddLine), None);
        let ansi = Theme::new(ColorDepth::Ansi16, Some((0, 0, 0)));
        assert_eq!(ansi.surface(Surface::User), None);
        assert_eq!(ansi.add().fg, Some(Color::Green));
    }

    #[test]
    fn no_colour_theme_uses_modifiers_only() {
        let none = Theme::new(ColorDepth::None, None);
        assert_eq!(none.accent().fg, None);
        assert!(none.accent().add_modifier.contains(Modifier::BOLD));
        assert!(none.dim().add_modifier.contains(Modifier::DIM));
        assert_eq!(none.add().fg, None);
    }

    #[test]
    fn light_and_dark_backgrounds_pick_their_tones() {
        let dark = Theme::new(ColorDepth::TrueColor, Some((0, 0, 0)));
        let light = Theme::new(ColorDepth::TrueColor, Some((255, 255, 255)));
        assert_eq!(dark.accent().fg, Some(Color::Rgb(47, 180, 203)));
        assert_eq!(light.accent().fg, Some(Color::Rgb(14, 127, 148)));
    }

    #[test]
    fn nearest_256_picks_cube_or_grey() {
        assert_eq!(nearest_256((0, 0, 0)), 16);
        assert_eq!(nearest_256((255, 255, 255)), 231);
        assert_eq!(nearest_256((128, 128, 128)), 244);
        assert_eq!(nearest_256((255, 0, 0)), 196);
    }
}
