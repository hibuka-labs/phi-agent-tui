//! Off-screen render snapshots (frame logs and tests).

use super::*;

/// Serialize a rendered buffer to a plain-text grid (one row per line).
///
/// Box-drawing borders and emoji markers survive as UTF-8; styles (color, bold)
/// are dropped. Used by the frame-capture log so a TUI session can be reviewed
/// afterward as a flipbook of text screens.
pub fn buffer_to_text(buf: &Buffer) -> String {
    let area = buf.area;
    let cells = buf.content();
    let width = area.width as usize;
    let mut out = String::with_capacity((width + 1) * area.height as usize);
    for y in 0..area.height {
        let mut line = String::new();
        let mut x = 0usize;
        while x < width {
            let sym = cells[y as usize * width + x].symbol();
            line.push_str(sym);
            // A wide glyph (CJK, etc.) is stored in one cell with the following
            // cell as a reset continuation; step over it so we don't emit a
            // stray space between wide chars.
            x += sym.width().max(1);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Render `app` offscreen at `width`×`height` and return it as text.
///
/// Mirrors the on-screen `draw` (same layout, status bar, and popups), so the
/// captured frames match what a live terminal shows.
pub fn snapshot_text(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("offscreen terminal");
    terminal.draw(|f| draw(f, app)).expect("offscreen draw");
    buffer_to_text(terminal.backend().buffer())
}

/// The rendered rows of the process line, with each cell's foreground color.
///
/// [`snapshot_text`] drops styles, so a test that asserts "these two pieces
/// of the row wear different colors" needs the colors themselves. Returns one
/// entry per cell of the row that carries the process line, as
/// `(text, foreground color)` runs — consecutive cells with the same color
/// are merged into one run.
pub fn snapshot_process_line_colors(
    app: &mut App,
    width: u16,
    height: u16,
) -> Vec<(String, Color)> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("offscreen terminal");
    terminal.draw(|f| draw(f, app)).expect("offscreen draw");
    let buf = terminal.backend().buffer();
    let area = buf.area;
    let cells = buf.content();
    let w = area.width as usize;
    // The process line is the row right above the composer — the last row
    // before the composer's own top rule. Scan for the row that carries the
    // status text instead of guessing an index.
    let mut runs: Vec<(String, Color)> = Vec::new();
    for y in 0..area.height {
        // Fresh runs per row — a run never spans two rows.
        runs.clear();
        let mut row = String::new();
        for x in 0..w {
            let cell = &cells[y as usize * w + x];
            let sym = cell.symbol();
            let fg = cell.fg;
            let last = runs.last_mut();
            match last {
                Some(run) if run.1 == fg => {
                    run.0.push_str(sym);
                }
                _ => runs.push((sym.to_string(), fg)),
            }
            row.push_str(sym);
        }
        if row.contains("thinking...") || row.contains("(Ctrl+C cancel)") {
            // Drop the empty leading run the buffer starts with.
            return runs
                .into_iter()
                .filter(|(t, _)| !t.trim().is_empty())
                .collect();
        }
    }
    Vec::new()
}
