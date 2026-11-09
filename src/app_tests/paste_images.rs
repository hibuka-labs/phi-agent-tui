//! The paste pipeline's App face: chips, the vision gate, the capture write,
//! and the rescue that hands screenshots back.

use std::sync::Arc;

use super::super::*;

fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode test png");
    out
}

/// protects: a pasted screenshot lands as a chip and its bytes land as a
/// capture under `captures/`, byte-identical. fails_when: the paste re-encodes
/// or files the capture outside the session dir. no_claim: what the model
/// does with the picture.
#[test]
fn a_pasted_screenshot_lands_on_the_chip_row() {
    let mut app = App::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = png_bytes(2, 3);

    let outcome = app.apply_pasted_image(Ok(Some(bytes.clone())), dir.path());
    assert_eq!(outcome, PasteOutcome::Attached);
    assert_eq!(app.pending_image_labels(), vec!["📷 2x3".to_string()]);

    let pending = &app.pending_images[0];
    assert_eq!((pending.width, pending.height), (2, 3));
    assert_eq!(pending.path.parent().unwrap(), dir.path().join("captures"));
    let on_disk = std::fs::read(&pending.path).expect("capture written");
    assert_eq!(on_disk, bytes, "bytes intact");
}

/// protects: the send consumes every chip (the submit site takes them into
/// the run). fails_when: chips survive a submit and would re-send. no_claim:
/// what the run does with them.
#[test]
fn the_chip_row_clears_with_the_send() {
    let mut app = App::new();
    let dir = tempfile::tempdir().expect("tempdir");
    app.apply_pasted_image(Ok(Some(png_bytes(2, 2))), dir.path());
    app.apply_pasted_image(Ok(Some(png_bytes(4, 4))), dir.path());
    assert_eq!(app.pending_images.len(), 2);

    let taken = app.take_pending_images();
    assert_eq!(taken.len(), 2);
    assert_eq!((taken[0].width, taken[1].width), (2, 4), "order kept");
    assert!(app.pending_images.is_empty());
    assert!(app.pending_image_labels().is_empty());
}

/// protects: Backspace on an empty composer eats the last chip, and with text
/// in the composer it stays a plain backspace (chips untouched). fails_when:
/// a backspace silently discards a screenshot while text remains, or eats
/// nothing when only chips are left. no_claim: chip order beyond "last goes
/// first".
#[test]
fn backspace_on_an_empty_composer_eats_the_last_chip() {
    let mut app = App::new();
    let dir = tempfile::tempdir().expect("tempdir");
    app.apply_pasted_image(Ok(Some(png_bytes(2, 2))), dir.path());
    app.apply_pasted_image(Ok(Some(png_bytes(3, 3))), dir.path());

    app.composer.insert_str("hi");
    app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(app.composer.text(), "h");
    assert_eq!(app.pending_images.len(), 2, "text backspace spares chips");

    app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert!(app.composer.is_empty());
    assert_eq!(app.pending_images.len(), 2, "the last text char goes first");

    app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(app.pending_images.len(), 1);
    // The last-pasted chip goes first — the first paste is the survivor.
    assert_eq!(app.pending_images[0].width, 2);
}

/// protects: non-images are refused before any write (magic bytes decide),
/// and the refusal is on screen, not a silent no-op. fails_when: HTML bytes
/// land in `captures/` or the composer shrugs. no_claim: wording beyond the
/// error marker.
#[test]
fn non_images_are_refused_before_any_write() {
    let mut app = App::new();
    let dir = tempfile::tempdir().expect("tempdir");

    let outcome =
        app.apply_pasted_image(Ok(Some(b"<html>not a picture</html>".to_vec())), dir.path());
    assert_eq!(outcome, PasteOutcome::Rejected);
    assert!(app.pending_images.is_empty());
    assert!(
        !dir.path().join("captures").exists(),
        "no capture was written"
    );

    // Magic bytes pass, but there is no readable header behind them.
    let mut fake = b"\x89PNG\r\n\x1a\n".to_vec();
    fake.extend_from_slice(&[0; 4]);
    let outcome = app.apply_pasted_image(Ok(Some(fake)), dir.path());
    assert_eq!(outcome, PasteOutcome::Rejected);
    assert!(app.pending_images.is_empty());
}

/// protects: the vision gate refuses before the capture is written — a model
/// that cannot see pictures gets words and a /model pointer (AC3's paste
/// face). fails_when: the picture is attached to a blind request, or refused
/// without pointing at the fix. no_claim: which models lack vision.
#[test]
fn the_vision_gate_refuses_before_the_capture() {
    let mut app = App::new();
    app.set_vision(Arc::new(|| Some(false)));
    let dir = tempfile::tempdir().expect("tempdir");

    let outcome = app.apply_pasted_image(Ok(Some(png_bytes(2, 2))), dir.path());
    assert_eq!(outcome, PasteOutcome::Rejected);
    assert!(app.pending_images.is_empty());
    assert!(!dir.path().join("captures").exists(), "no capture written");

    // A seeing model (and an unknown probe) passes.
    app.set_vision(Arc::new(|| Some(true)));
    assert_eq!(
        app.apply_pasted_image(Ok(Some(png_bytes(2, 2))), dir.path()),
        PasteOutcome::Attached
    );
    let mut app = App::new();
    assert_eq!(
        app.apply_pasted_image(Ok(Some(png_bytes(2, 2))), dir.path()),
        PasteOutcome::Attached,
        "an unknown probe never blocks the paste"
    );
}

/// protects: a grabber failure is shown as an error line (AC6: errors are
/// 明示, never swallowed into "no image"); "no image" is the only outcome
/// that falls through to text paste. fails_when: an error reads as silence.
/// no_claim: the helper's own wording.
#[test]
fn helper_failures_are_shown_not_swallowed() {
    let mut app = App::new();
    let dir = tempfile::tempdir().expect("tempdir");

    let outcome = app.apply_pasted_image(Err("boom".to_string()), dir.path());
    assert_eq!(outcome, PasteOutcome::Rejected);
    let shown = app
        .transcript
        .output
        .iter()
        .any(|l| l.text.contains("boom"));
    assert!(shown, "the failure is on screen");

    assert_eq!(
        app.apply_pasted_image(Ok(None), dir.path()),
        PasteOutcome::NoImage,
        "an empty clipboard is the text-paste fallback, not an error"
    );
}

/// protects: a rescued submit hands the screenshots back — Ctrl+C before any
/// output restores the whole draft (chips included). fails_when: a rescue
/// returns the text but loses the attachments. no_claim: the rescue's
//  timing rules (pinned in turn_state tests).
#[test]
fn a_rescued_submit_puts_the_chips_back() {
    let mut app = App::new();
    let dir = tempfile::tempdir().expect("tempdir");
    app.apply_pasted_image(Ok(Some(png_bytes(7, 7))), dir.path());
    let images = app.take_pending_images();

    app.begin_rescuable_turn_with_images("look here", &images);
    app.running = true;
    assert!(
        !app.transcript.output.is_empty(),
        "the echo (text + chip line) landed"
    );

    let text = app.take_cancel_rescue().expect("rescuable");
    assert_eq!(text, "look here");
    assert_eq!(app.pending_images.len(), 1, "the chip came back");
    assert_eq!(app.pending_images[0].width, 7);
}
