//! Contract tests for the task panel's pure layout/wording layer: the glyph
//! table, the FAILED conditional format (never invent a number 绝不编数字), the ledger marker
//! round-trip, and the squeeze ladder (the hint disappears last 提示最后消失).

use std::time::Duration;

use phi_tui::wrap::{Elide, elide};
use unicode_width::UnicodeWidthStr;

use super::*;

fn w(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

// ── glyph table ──────────────────────────────────────────────────────────

#[test]
fn glyph_table_matches_design() {
    assert_eq!(
        status_glyph(&SubAgentStatus::Running),
        ("*", RowTint::Running)
    );
    assert_eq!(status_glyph(&SubAgentStatus::Done), ("+", RowTint::Done));
    assert_eq!(
        status_glyph(&SubAgentStatus::Failed {
            summary: "boom".into()
        }),
        ("!", RowTint::Error)
    );
    assert_eq!(
        status_glyph(&SubAgentStatus::Failed {
            summary: "task timed out after 30s".into()
        }),
        ("!", RowTint::Warn)
    );
    assert_eq!(
        status_glyph(&SubAgentStatus::Cancelled),
        ("-", RowTint::Warn)
    );
}

#[test]
fn timeout_summary_is_only_the_producer_prefix() {
    assert!(is_timeout_summary("task timed out after 30s"));
    assert!(!is_timeout_summary("spawn: task timed out"));
    assert!(!is_timeout_summary("timed out"));
    assert!(!is_timeout_summary(""));
}

// ── exit-code extraction (never invent a number) ──────────────────────────

#[test]
fn exit_code_all_literal_forms() {
    assert_eq!(
        extract_exit_code("process failed with exit code 2"),
        Some(2)
    );
    assert_eq!(extract_exit_code("exit status 3"), Some(3));
    assert_eq!(extract_exit_code("exited with code 4"), Some(4));
    assert_eq!(extract_exit_code("exit 5"), Some(5));
    assert_eq!(extract_exit_code("Exit Code 7"), Some(7));
    assert_eq!(extract_exit_code("exited with code -2"), Some(-2));
    assert_eq!(extract_exit_code("exit status 0"), Some(0));
}

#[test]
fn exit_code_never_fabricates_on_a_miss() {
    assert_eq!(extract_exit_code("no code here"), None);
    assert_eq!(extract_exit_code("exit code"), None);
    assert_eq!(extract_exit_code("boom exit code"), None);
    assert_eq!(extract_exit_code(""), None);
}

#[test]
fn exit_code_respects_word_boundaries() {
    // A letter glued to the front of "exit " is not the form.
    assert_eq!(extract_exit_code("sexit 4"), None);
    assert_eq!(extract_exit_code("hexit 4"), None);
    // …but punctuation is a legal boundary.
    assert_eq!(extract_exit_code("(exit 4)"), Some(4));
}

#[test]
fn exit_code_takes_the_earliest_form() {
    assert_eq!(extract_exit_code("exit code 2 and exit status 5"), Some(2));
    assert_eq!(extract_exit_code("exit 5 and exit code 2"), Some(5));
}

#[test]
fn exit_code_scans_past_an_unparseable_digit_run() {
    // The absurd run overflows i32; the real code later must still be found.
    assert_eq!(
        extract_exit_code("exit 99999999999999999999 then exit 1"),
        Some(1)
    );
}

// ── FAILED conditional format ────────────────────────────────────────────

#[test]
fn failed_label_prefers_the_exit_code() {
    assert_eq!(
        failed_label("process failed with exit code 2"),
        "FAILED (exit 2)"
    );
}

#[test]
fn failed_label_falls_back_to_the_summary_tail() {
    let summary = "the recursive descent blew up deep in the parser stack";
    let label = failed_label(summary);
    let flat = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let inner = elide(&flat, FAILED_SUMMARY_COLS, Elide::Tail { sep: None });
    assert_eq!(label, format!("FAILED ({inner})"));
    // The tail carries the reason — it must survive the squeeze.
    assert!(label.ends_with("parser stack)"), "tail kept, got {label}");
}

#[test]
fn failed_label_never_fabricates_an_exit_form() {
    let label = failed_label("boom exit code");
    assert!(
        !label.contains("(exit "),
        "no invented (exit N), got {label}"
    );
    assert_eq!(label, "FAILED (boom exit code)");
}

#[test]
fn failed_label_bare_and_flattened() {
    assert_eq!(failed_label(""), "FAILED");
    assert_eq!(failed_label("   "), "FAILED");
    assert_eq!(failed_label("  a   b  "), "FAILED (a b)");
}

#[test]
fn failed_label_summary_fits_unelided_at_24_cols() {
    let summary = "x".repeat(FAILED_SUMMARY_COLS);
    assert_eq!(failed_label(&summary), format!("FAILED ({summary})"));
    let long = "x".repeat(FAILED_SUMMARY_COLS + 1);
    let label = failed_label(&long);
    assert!(w(label.trim_start_matches("FAILED (").trim_end_matches(')')) <= FAILED_SUMMARY_COLS);
}

// ── durations, ledger lines, marker round-trip ───────────────────────────

#[test]
fn format_time_panel_form() {
    assert_eq!(format_time(Duration::from_secs(0)), "0s");
    assert_eq!(format_time(Duration::from_secs(59)), "59s");
    assert_eq!(format_time(Duration::from_secs(60)), "1m0s");
    assert_eq!(format_time(Duration::from_secs(133)), "2m13s");
    assert_eq!(format_time(Duration::from_secs(3599)), "59m59s");
}

#[test]
fn ledger_lines_cover_every_terminal() {
    let started = ledger_line("root/a", &SubAgentStatus::Running, Duration::from_secs(3));
    assert_eq!(started, "* [root/a] started");

    let done = ledger_line("root/a", &SubAgentStatus::Done, Duration::from_secs(133));
    assert_eq!(done, "* [root/a] done (2m13s)");

    let failed = ledger_line(
        "root/a",
        &SubAgentStatus::Failed {
            summary: "process failed with exit code 2".into(),
        },
        Duration::from_secs(1),
    );
    assert_eq!(failed, "* [root/a] FAILED (exit 2)");

    let bare = ledger_line(
        "root/a",
        &SubAgentStatus::Failed {
            summary: String::new(),
        },
        Duration::from_secs(1),
    );
    assert_eq!(bare, "* [root/a] FAILED");

    let cancelled = ledger_line("root/a", &SubAgentStatus::Cancelled, Duration::from_secs(1));
    assert_eq!(cancelled, "* [root/a] CANCELLED");
}

#[test]
fn lead_piece_trims() {
    let waiting = LeadPieces {
        head: "waiting...".into(),
        activity: " - 42s".into(),
        tail: " auto-continue when results arrive".into(),
        tail_is_reassurance: true,
    };
    assert_eq!(
        waiting.full(),
        "waiting... - 42s auto-continue when results arrive"
    );
    assert_eq!(
        waiting.no_activity(),
        "waiting... auto-continue when results arrive"
    );
    assert_eq!(waiting.min(), "waiting...");

    // A phase lead's tail is the cancel hint — it survives every stage.
    let phase = LeadPieces {
        head: "executing".into(),
        activity: " - 7s".into(),
        tail: " (Ctrl+C cancel)".into(),
        tail_is_reassurance: false,
    };
    assert_eq!(phase.min(), "executing (Ctrl+C cancel)");
}

// ── process-row squeeze ladder (the hint disappears last) ──────────────────

fn lead_fixture() -> LeadPieces {
    LeadPieces {
        head: "⠋ waiting for sub-agents (3 running)...".into(),
        activity: " - 42s - 15.0k tok".into(),
        tail: " auto-continue when results arrive".into(),
        tail_is_reassurance: true,
    }
}

#[test]
fn fit_lead_drops_the_tip_before_any_text() {
    let lead = lead_fixture();
    let wide = lead.full();
    // Stage 0: everything wide, Tip rides.
    let fit = fit_lead(&lead, w(&wide) + TIP_GAP + TIP_MIN_COLS);
    assert_eq!(fit.text, wide);
    assert!(fit.tip_allowed);

    // One column less and the Tip drops before any text degrades.
    let fit = fit_lead(&lead, w(&wide) + TIP_GAP + TIP_MIN_COLS - 1);
    assert_eq!(fit.text, wide);
    assert!(!fit.tip_allowed);
}

#[test]
fn fit_lead_degrades_in_design_order() {
    let lead = lead_fixture();
    // ① activity suffix gone.
    let fit = fit_lead(&lead, w(&lead.no_activity()));
    assert_eq!(fit.text, lead.no_activity());
    assert!(!fit.tip_allowed);

    // ② reassurance tail gone (waiting leads only — phase leads keep theirs).
    let fit = fit_lead(&lead, w(&lead.min()));
    assert_eq!(fit.text, lead.min());

    let phase = LeadPieces {
        head: "executing".into(),
        activity: " - 7s".into(),
        tail: " (Ctrl+C cancel)".into(),
        tail_is_reassurance: false,
    };
    let fit = fit_lead(&phase, w(&phase.min()));
    assert_eq!(fit.text, "executing (Ctrl+C cancel)");

    // Beyond the ladder: hard-cut from the head's tail (weak tenants at the tail).
    let fit = fit_lead(&lead, 4);
    assert_eq!(fit.text, elide(&lead.min(), 4, Elide::Head));
    assert!(!fit.tip_allowed);
}

#[test]
fn fit_lead_fits_at_every_returned_width() {
    let lead = lead_fixture();
    for width in 1..=w(&lead.full()) + 20 {
        let fit = fit_lead(&lead, width);
        assert!(
            w(&fit.text) <= width,
            "overflow at width {width}: {:?}",
            fit.text
        );
    }
}

// ── frame height / window ────────────────────────────────────────────────

#[test]
fn frame_height_full_shrink_suppress() {
    assert_eq!(frame_height(0, 10), 3); // one content row is the floor
    assert_eq!(frame_height(1, 10), 3);
    assert_eq!(frame_height(6, 10), 8); // 6 + 2 borders
    assert_eq!(frame_height(20, 10), 8); // content caps at 6
    assert_eq!(frame_height(6, 8), 8);
    assert_eq!(frame_height(6, 5), 5); // shrink to what fits
    assert_eq!(frame_height(6, 3), 3); // border + 1 content is the floor
    assert_eq!(frame_height(6, 2), 0); // suppressed: status line survives
    assert_eq!(frame_height(0, 0), 0);
}

#[test]
fn window_offset_defaults_to_the_tail() {
    assert_eq!(window_offset(0, None, 20, 6), 14);
    assert_eq!(window_offset(0, None, 3, 6), 0);
    assert_eq!(window_offset(0, None, 0, 6), 0);
}

#[test]
fn window_offset_minimal_movement_and_clamps() {
    // Selection above the window pulls it up; below pushes it down.
    assert_eq!(window_offset(5, Some(1), 20, 6), 1);
    assert_eq!(window_offset(5, Some(11), 20, 6), 6);
    // Already visible → the window does not move at all.
    assert_eq!(window_offset(5, Some(9), 20, 6), 5);
    // Out-of-range targets fall back to the tail; offsets clamp.
    assert_eq!(window_offset(0, Some(25), 20, 6), 14);
    assert_eq!(window_offset(99, None, 20, 6), 14);
}

#[test]
fn window_offset_keeps_every_row_reachable() {
    // 20 rows through a 6-row window: walking down and back up must visit all.
    let (mut off, mut seen) = (0, vec![false; 20]);
    for (t, seen_row) in seen.iter_mut().enumerate() {
        off = window_offset(off, Some(t), 20, 6);
        assert!(
            off <= t && t < off + 6,
            "row {t} not visible at offset {off}"
        );
        *seen_row = true;
    }
    for t in (0..20).rev() {
        off = window_offset(off, Some(t), 20, 6);
        assert!(
            off <= t && t < off + 6,
            "row {t} not visible at offset {off}"
        );
    }
    assert!(seen.iter().all(|s| *s));
}

// ── live face (Rev 2026-11-04) ───────────────────────────────────────────

#[test]
fn panel_title_badge_only_while_unacked() {
    assert_eq!(panel_title(4, 0), "Tasks (4)");
    assert_eq!(panel_title(4, 2), "Tasks (4) · 2 new");
}

#[test]
fn live_face_height_caps_shrinks_suppresses() {
    assert_eq!(live_face_height(3, false, 10), 3);
    assert_eq!(live_face_height(3, true, 10), 4); // overflow line under the rows
    assert_eq!(live_face_height(9, true, 10), 7); // rows cap at 6
    assert_eq!(live_face_height(3, true, 3), 3); // shrink to what fits
    assert_eq!(live_face_height(3, true, 0), 0); // suppressed
}

#[test]
fn live_row_widths_strip_the_files_column() {
    assert_eq!(live_row_widths(40, 16), (16, 24));
    assert_eq!(live_row_widths(20, 5), (12, 8)); // name floor 12
    assert_eq!(live_row_widths(20, 100), (16, 4)); // name cap 16
    // Physics: the name alone may eat the whole budget.
    assert_eq!(live_row_widths(10, 16), (10, 0));
    assert_eq!(live_row_widths(0, 16), (0, 0));
}

#[test]
fn overflow_line_counts_and_degrades_tail_first() {
    assert_eq!(overflow_line(0, 0, 80), None); // nothing hidden → no line
    assert_eq!(
        overflow_line(3, 2, 80),
        Some("+ 3 more · + 2 done · ^T tasks".to_string())
    );
    assert_eq!(
        overflow_line(3, 0, 80),
        Some("+ 3 more · ^T tasks".to_string())
    );
    assert_eq!(
        overflow_line(0, 2, 80),
        Some("+ 2 done · ^T tasks".to_string())
    );
    // The hint dies first (discoverability, not data), then the done count.
    assert_eq!(
        overflow_line(3, 2, w("+ 3 more · + 2 done")),
        Some("+ 3 more · + 2 done".to_string())
    );
    assert_eq!(
        overflow_line(3, 2, w("+ 3 more")),
        Some("+ 3 more".to_string())
    );
    // Physics: hard-cut from the head's tail.
    let got = overflow_line(30, 2, 5).unwrap();
    assert!(w(&got) <= 5, "overflow line overshoots: {got:?}");
}

// ── row columns ──────────────────────────────────────────────────────────

#[test]
fn column_widths_squeeze_right_to_left() {
    // Comfortable: activity is the flex column and takes the slack.
    assert_eq!(column_widths(40, 16), (16, 8, 16));
    assert_eq!(column_widths(30, 5), (12, 8, 10)); // name floor 12
    assert_eq!(column_widths(30, 100), (16, 8, 6)); // name cap 16
    // Activity shrinks to zero first…
    assert_eq!(column_widths(25, 16), (16, 8, 1));
    assert_eq!(column_widths(24, 16), (16, 8, 0));
    // …then the files column…
    assert_eq!(column_widths(18, 16), (16, 2, 0));
    assert_eq!(column_widths(20, 16), (16, 4, 0));
    // …then the name to its 12-column floor; physics wins below that.
    assert_eq!(column_widths(12, 16), (12, 0, 0));
    assert_eq!(column_widths(10, 16), (10, 0, 0));
}

#[test]
fn column_widths_never_overshoot_the_panel() {
    for inner in 0..=48 {
        let (name, files, act) = column_widths(inner, 16);
        assert!(
            name + files + act <= inner.max(name),
            "overshoot at inner {inner}"
        );
    }
}

#[test]
fn files_and_activity_cells() {
    assert_eq!(files_cell(1), "1 file");
    assert_eq!(files_cell(0), "0 files");
    assert_eq!(files_cell(3), "3 files");

    let run = Duration::from_secs(133);
    assert_eq!(activity_cell(&SubAgentStatus::Running, run), "");
    assert_eq!(activity_cell(&SubAgentStatus::Done, run), "done 2m13s");
    assert_eq!(
        activity_cell(
            &SubAgentStatus::Failed {
                summary: "process failed with exit code 2".into()
            },
            run
        ),
        "FAILED (exit 2)"
    );
    assert_eq!(
        activity_cell(
            &SubAgentStatus::Failed {
                summary: String::new()
            },
            run
        ),
        "FAILED"
    );
    assert_eq!(activity_cell(&SubAgentStatus::Cancelled, run), "cancelled");
}

#[test]
fn cells_fit_their_column_cjk_included() {
    assert_eq!(cell("hello", 8, Elide::Head), "hello   ");
    let cut = cell("abcdefghij", 6, Elide::Head);
    assert_eq!(cut, "abc...");
    let cjk = cell("分析报告", 6, Elide::Tail { sep: None });
    assert_eq!(w(&cjk), 6);
}
