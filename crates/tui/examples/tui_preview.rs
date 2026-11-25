//! Prints the preview scenes as JSON cells (see `preview_support`):
//! `LYNSHEN_LANG=zh cargo run -p lynshen-tui --example tui_preview --features preview`.

fn main() {
    println!("{}", lynshen_tui::preview_support::render_all());
}
