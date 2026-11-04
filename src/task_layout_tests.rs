//! Contract tests for the task panel's pure layout/wording layer: the glyph
//! table, the FAILED conditional format (绝不编数字), the ledger marker
//! round-trip, and the squeeze ladder (提示最后消失).

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

// ── exit-code extraction (绝不编数字) ────────────────────────────────────

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
fn ledger_round_trip_covers_every_terminal() {
    let done = ledger_line("root/a", &SubAgentStatus::Done, Duration::from_secs(133));
    assert_eq!(done, "* [root/a] done (2m13s)");
    assert_eq!(parse_ledger_marker(&done), Some(LedgerMark::Done));

    let failed = ledger_line(
        "root/a",
        &SubAgentStatus::Failed {
            summary: "process failed with exit code 2".into(),
        },
        Duration::from_secs(1),
    );
    assert_eq!(failed, "* [root/a] FAILED (exit 2)");
    assert_eq!(parse_ledger_marker(&failed), Some(LedgerMark::Failed));

    let bare = ledger_line(
        "root/a",
        &SubAgentStatus::Failed {
            summary: String::new(),
        },
        Duration::from_secs(1),
    );
    assert_eq!(bare, "* [root/a] FAILED");
    assert_eq!(parse_ledger_marker(&bare), Some(LedgerMark::Failed));

    let cancelled = ledger_line("root/a", &SubAgentStatus::Cancelled, Duration::from_secs(1));
    assert_eq!(cancelled, "* [root/a] CANCELLED");
    assert_eq!(parse_ledger_marker(&cancelled), Some(LedgerMark::Cancelled));
}

#[test]
fn ledger_started_line_and_noise_are_not_markers() {
    assert_eq!(parse_ledger_marker("* [root/a] started"), None);
    assert_eq!(parse_ledger_marker("* [root/a]"), None);
    assert_eq!(parse_ledger_marker("done"), None);
    assert_eq!(parse_ledger_marker("* [root/a] doneish"), None);
    assert_eq!(parse_ledger_marker(""), None);
}

// ── counts, hint, segment ────────────────────────────────────────────────

#[test]
fn counts_record_buckets_and_never_shrink() {
    let mut c = TaskCounts::default();
    c.record(&SubAgentStatus::Running); // not a terminal — no bucket
    assert_eq!(c, TaskCounts::default());
    c.record(&SubAgentStatus::Done);
    c.record(&SubAgentStatus::Failed {
        summary: "task timed out after 1s".into(),
    }); // timeouts are ✗
    c.record(&SubAgentStatus::Failed {
        summary: "boom".into(),
    });
    c.record(&SubAgentStatus::Cancelled);
    assert_eq!(
        c,
        TaskCounts {
            done: 1,
            failed: 2,
            cancelled: 1
        }
    );
    assert_eq!(c.total(), 4);
}

#[test]
fn counts_blocks_match_the_string_table() {
    let c = TaskCounts {
        done: 18,
        failed: 2,
        cancelled: 1,
    };
    assert_eq!(
        c.full_block(2),
        " · 23 total · 2 running · 18 done · 2 failed · 1 cancelled"
    );
    assert_eq!(c.compact_block(2), " · 23t 2r 18d 2f 1c");

    // The cancelled bucket rides only when nonzero (D9).
    let clean = TaskCounts {
        done: 3,
        failed: 0,
        cancelled: 0,
    };
    assert_eq!(
        clean.full_block(0),
        " · 3 total · 0 running · 3 done · 0 failed"
    );
    assert_eq!(clean.compact_block(0), " · 3t 0r 3d 0f");
}

#[test]
fn hint_block_two_states_and_short() {
    assert_eq!(hint_block(false, false), " · Ctrl+T tasks");
    assert_eq!(hint_block(true, false), " · Ctrl+T close");
    assert_eq!(hint_block(false, true), " · ^T tasks");
    assert_eq!(hint_block(true, true), " · ^T close");
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

#[test]
fn compose_status_is_the_string_table_wording() {
    let lead = LeadPieces {
        head: "⠋ waiting for sub-agents (2 running)...".into(),
        activity: " - 12s".into(),
        tail: " auto-continue when results arrive".into(),
        tail_is_reassurance: true,
    };
    let seg = TaskSegment {
        counts: TaskCounts {
            done: 3,
            failed: 1,
            cancelled: 0,
        },
        running: 2,
        frame_open: false,
        show: true,
    };
    assert_eq!(
        compose_status(&lead, &seg),
        "⠋ waiting for sub-agents (2 running)... - 12s auto-continue when results arrive \
         · 6 total · 2 running · 3 done · 1 failed · Ctrl+T tasks"
    );

    let open = TaskSegment {
        frame_open: true,
        ..seg.clone()
    };
    assert!(compose_status(&lead, &open).ends_with("· Ctrl+T close"));

    // No task history → the process row stays untouched.
    let hidden = TaskSegment {
        show: false,
        ..seg.clone()
    };
    assert_eq!(compose_status(&lead, &hidden), lead.full());
}

// ── squeeze ladder (提示最后消失) ────────────────────────────────────────

fn ladder_fixture() -> (LeadPieces, TaskSegment, [String; 5]) {
    let lead = LeadPieces {
        head: "⠋ waiting for sub-agents (3 running)...".into(),
        activity: " - 42s - 15.0k tok".into(),
        tail: " auto-continue when results arrive".into(),
        tail_is_reassurance: true,
    };
    let seg = TaskSegment {
        counts: TaskCounts {
            done: 18,
            failed: 2,
            cancelled: 1,
        },
        running: 3,
        frame_open: false,
        show: true,
    };
    let counts_full = seg.counts.full_block(seg.running);
    let counts_compact = seg.counts.compact_block(seg.running);
    let hint = hint_block(false, false);
    let hint_short = hint_block(false, true);
    // Ladder rungs, widest first: wide / ① compact / ③a no-activity /
    // ③b min + full hint / ④ min + short hint / (hint dead).
    let rungs = [
        format!("{}{}{}", lead.full(), counts_full, hint),
        format!("{}{}{}", lead.full(), counts_compact, hint),
        format!("{}{}{}", lead.no_activity(), counts_compact, hint),
        format!("{}{}{}", lead.min(), counts_compact, hint_short),
        format!("{}{}{}", lead.min(), counts_compact, ""),
    ];
    (lead, seg, rungs)
}

#[test]
fn squeeze_ladder_degrades_in_design_order() {
    let (lead, seg, [wide, compact, no_act, short_hint, no_hint]) = ladder_fixture();
    let min_full_hint = format!(
        "{}{}{}",
        lead.min(),
        seg.counts.compact_block(seg.running),
        hint_block(false, false)
    );
    assert!(w(&wide) > w(&compact), "wide must lose to ①");
    assert!(w(&compact) > w(&no_act), "① must lose to ③a");
    assert!(w(&no_act) > w(&min_full_hint), "③a must lose to ③b");
    assert!(w(&min_full_hint) > w(&short_hint), "③b must lose to ④");
    assert!(w(&short_hint) > w(&no_hint), "④ must lose to the dead hint");

    // Stage 0: everything wide, Tip rides.
    let fit = fit_status(&lead, &seg, w(&wide) + TIP_GAP + TIP_MIN_COLS);
    assert_eq!(fit.text, wide);
    assert!(fit.tip_allowed);

    // One column less and ① keeps the Tip.
    let fit = fit_status(&lead, &seg, w(&wide) + TIP_GAP + TIP_MIN_COLS - 1);
    assert_eq!(fit.text, compact);
    assert!(fit.tip_allowed);

    // ②: the Tip drops before any text degrades.
    let fit = fit_status(&lead, &seg, w(&compact) + TIP_GAP + TIP_MIN_COLS - 1);
    assert_eq!(fit.text, compact);
    assert!(!fit.tip_allowed);

    // ③a: activity suffix gone.
    let fit = fit_status(&lead, &seg, w(&compact) - 1);
    assert_eq!(fit.text, no_act);
    assert!(!fit.tip_allowed);

    // ③b: reassurance tail gone, hint still full.
    let fit = fit_status(&lead, &seg, w(&min_full_hint));
    assert_eq!(fit.text, min_full_hint);
    assert!(!fit.tip_allowed);

    // ④: hint shrinks to ^T.
    let fit = fit_status(&lead, &seg, w(&short_hint));
    assert_eq!(fit.text, short_hint);
    assert!(fit.text.contains("^T tasks"));

    // The hint dies last: one column less and only the hint is gone.
    let fit = fit_status(&lead, &seg, w(&short_hint) - 1);
    assert_eq!(fit.text, no_hint);
    assert!(
        !fit.text.contains("^T"),
        "hint is the last sacrifice: {}",
        fit.text
    );
    assert!(!fit.text.contains("Ctrl+T"));

    // Beyond the ladder: hard-cut from the tail (weak tenants sit at the tail).
    let fit = fit_status(&lead, &seg, 4);
    assert_eq!(fit.text, elide(&no_hint, 4, Elide::Head));
    assert!(!fit.tip_allowed);
}

#[test]
fn squeeze_ladder_fits_at_every_returned_width() {
    let (lead, seg, _) = ladder_fixture();
    for width in 1..=w(&lead.full()) + seg.counts.full_block(seg.running).len() + 20 {
        let fit = fit_status(&lead, &seg, width);
        assert!(
            w(&fit.text) <= width,
            "overflow at width {width}: {:?}",
            fit.text
        );
    }
}

#[test]
fn segment_without_history_is_never_mounted() {
    let (lead, _, _) = ladder_fixture();
    let seg = TaskSegment::default(); // show = false
    let fit = fit_status(&lead, &seg, 200);
    assert_eq!(fit.text, lead.full());
    assert!(fit.tip_allowed);
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
    assert_eq!(window_offset(0, None, None, 20, 6), 14);
    assert_eq!(window_offset(0, None, None, 3, 6), 0);
    assert_eq!(window_offset(0, None, None, 0, 6), 0);
}

#[test]
fn window_offset_minimal_movement_and_clamps() {
    // Selection above the window pulls it up; below pushes it down.
    assert_eq!(window_offset(5, Some(1), None, 20, 6), 1);
    assert_eq!(window_offset(5, Some(11), None, 20, 6), 6);
    // Already visible → the window does not move at all.
    assert_eq!(window_offset(5, Some(9), None, 20, 6), 5);
    // Selection wins over the S1 anchor.
    assert_eq!(window_offset(10, Some(1), Some(15), 20, 6), 1);
    // Anchor alone drives the window when no row is selected.
    assert_eq!(window_offset(10, None, Some(3), 20, 6), 3);
    // Out-of-range targets fall back to the tail; offsets clamp.
    assert_eq!(window_offset(0, Some(25), None, 20, 6), 14);
    assert_eq!(window_offset(99, None, None, 20, 6), 14);
}

#[test]
fn window_offset_keeps_every_row_reachable() {
    // 20 rows through a 6-row window: walking down and back up must visit all.
    let (mut off, mut seen) = (0, vec![false; 20]);
    for (t, seen_row) in seen.iter_mut().enumerate() {
        off = window_offset(off, Some(t), None, 20, 6);
        assert!(
            off <= t && t < off + 6,
            "row {t} not visible at offset {off}"
        );
        *seen_row = true;
    }
    for t in (0..20).rev() {
        off = window_offset(off, Some(t), None, 20, 6);
        assert!(
            off <= t && t < off + 6,
            "row {t} not visible at offset {off}"
        );
    }
    assert!(seen.iter().all(|s| *s));
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
