//! Terminal guard (restore on any exit) and exit notices.

use super::*;

/// Restores the terminal (raw mode + alternate screen) when dropped.
pub(super) struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
            LeaveAlternateScreen
        );
    }
}

/// One-line startup notice for Apple Terminal users. Terminal.app forwards
/// wheel/trackpad events to the app only while View → Allow Mouse Reporting
/// is checked for that window (the macOS default is ON — an unchecked window
/// is almost always a prior ⌘R/“refresh” toggle). When it is off, the swipe
/// is swallowed by the terminal itself and scrolling silently dies.
/// `term_program` is the value of `$TERM_PROGRAM`; only `Apple_Terminal`
/// gets the notice.
pub(super) fn terminal_scroll_notice(term_program: Option<&str>) -> Option<String> {
    if term_program != Some("Apple_Terminal") {
        return None;
    }
    Some(
        "Note: wheel/trackpad scroll needs View > Allow Mouse Reporting - fn+Up/fn+Down page history".to_string(),
    )
}

/// Exit-time diagnostic. Printed to the restored terminal when an entire run
/// in Apple Terminal never received a single wheel event even though the
/// transcript was taller than one screen — the exact fingerprint of Allow
/// Mouse Reporting being off in that window. Turns the previously silent
/// failure into an actionable message right after the session ends.
pub(super) fn wheel_dead_notice(
    term_program: Option<&str>,
    saw_scroll: bool,
    scrollable: bool,
) -> Option<String> {
    if term_program != Some("Apple_Terminal") || saw_scroll || !scrollable {
        return None;
    }
    Some(
        "Note: no wheel events were received this run. If trackpad scrolling does nothing, enable View > Allow Mouse Reporting in your terminal and retry; fn+Up/fn+Down always work".to_string(),
    )
}
