//! Prints the preview scenes as JSON cells (see `preview_support`):
//! `LYNSHEN_LANG=zh cargo run -p lynshen-tui --example tui_preview --features preview`.
//!
//! `indicator START_MS STEP_MS COUNT WIDTH HEIGHT` prints the working
//! indicator over time instead; `indicator-variants AT_MS WIDTH HEIGHT` one
//! moment of it under each colour and glyph fallback; `indicator-states
//! STATE_MS STEP_MS WIDTH HEIGHT` its eight motions in turn.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let number = |index: usize, default: u64| {
        args.get(index)
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let frames = match args.first().map(String::as_str) {
        Some("indicator") => lynshen_tui::preview_support::render_indicator_frames(
            number(1, 38_000),
            number(2, 83),
            number(3, 145),
            number(4, 72) as u16,
            number(5, 10) as u16,
        ),
        Some("indicator-states") => lynshen_tui::preview_support::render_indicator_states(
            number(1, 2_400),
            number(2, 83),
            number(3, 72) as u16,
            number(4, 10) as u16,
        ),
        Some("indicator-variants") => lynshen_tui::preview_support::render_indicator_variants(
            number(1, 41_400),
            number(2, 72) as u16,
            number(3, 10) as u16,
        ),
        _ => lynshen_tui::preview_support::render_all(),
    };
    println!("{frames}");
}
