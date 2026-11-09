//! The paste chip row above the composer.

use super::super::*;
use super::common::*;

fn chip(app: &mut App, w: u32, h: u32) {
    app.pending_images.push(crate::app::PendingImage {
        path: std::path::PathBuf::from("captures/paste.png"),
        width: w,
        height: h,
    });
}

/// protects: every pending screenshot renders as its `📷 WxH` chip on a row
/// directly above the composer. fails_when: a pasted picture leaves no trace
/// in the frame before it is sent. no_claim: the chip's color.
#[test]
fn pending_chips_render_above_the_composer() {
    let mut app = populated_app();
    chip(&mut app, 37, 11);
    chip(&mut app, 4, 4);

    let text = snapshot_text(&mut app, 80, 24);
    let chip_line = text
        .lines()
        .position(|l| l.contains("📷 37x11"))
        .expect("first chip row on screen");
    assert!(
        text.lines().any(|l| l.contains("📷 4x4")),
        "second chip row on screen:\n{text}"
    );
    let composer_line = text
        .lines()
        .position(|l| l.contains("hello"))
        .expect("composer on screen");
    assert!(
        chip_line < composer_line,
        "chips sit above the composer:\n{text}"
    );
}

/// protects: no chips, no chip row — the frame is byte-for-byte the plain
/// layout. fails_when: an empty attachment set leaves ghost rows.
#[test]
fn an_empty_chip_row_renders_nothing() {
    let mut app = populated_app();
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        !text.contains('📷'),
        "no chip rows without pending images:\n{text}"
    );
}
