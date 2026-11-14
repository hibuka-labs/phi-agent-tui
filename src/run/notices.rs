//! Terminal guard (enter/restore the drawing screen) and exit notices.

use super::*;

/// Restores the terminal (raw mode + alternate screen) when dropped.
pub(super) struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        restore_terminal_mode(&mut io::stdout());
    }
}

/// Take over the screen: alternate screen + mouse capture + bracketed paste
/// (fatal — the TUI has no screen without them), then the kitty
/// keyboard-protocol flags (progressive enhancement).
pub(super) fn enter_terminal_mode(out: &mut impl io::Write) -> io::Result<()> {
    execute!(
        out,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    // Kitty keyboard protocol: lets crossterm read the Command (Super) modifier
    // so Cmd+C can be bound to copy. Terminals that don't support it (e.g. the
    // macOS Terminal.app) ignore this and keep legacy key reporting — and the
    // legacy Windows console API cannot express it at all, answering with
    // "Keyboard progressive enhancement not implemented for the legacy Windows
    // API." Either way the TUI must still start: the push is best-effort.
    if let Err(err) = execute!(
        out,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    ) {
        tracing::debug!(%err, "keyboard enhancement unavailable; keeping legacy key reporting");
    }
    Ok(())
}

/// Give the screen back: undo [`enter_terminal_mode`]. Each step is its own
/// `execute!`: the macro chains its commands with `and_then`, so one failing
/// command aborts the rest — a refused `PopKeyboardEnhancementFlags` (the
/// legacy Windows API refuses it) used to eat the `LeaveAlternateScreen`
/// behind it and strand the user on the blank alternate screen after exit.
pub(super) fn restore_terminal_mode(out: &mut impl io::Write) {
    let _ = execute!(out, DisableMouseCapture);
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    let _ = execute!(out, LeaveAlternateScreen);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};

    /// Writer that refuses the first write completing `needle`, mimicking
    /// crossterm's legacy Windows API, which answers `Push/PopKeyboard-
    /// EnhancementFlags` with `Err("Keyboard progressive enhancement not
    /// implemented for the legacy Windows API.")` instead of emitting their
    /// escape sequence. Matching spans fragment boundaries because
    /// `write!(f, ...)` may split one command across several `write_str`s.
    struct RefuseOn<'a> {
        written: Vec<u8>,
        needle: &'a [u8],
        refused: bool,
    }

    impl Write for RefuseOn<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let carry = self.needle.len().saturating_sub(1);
            let tail_start = self.written.len().saturating_sub(carry);
            let mut probe = self.written[tail_start..].to_vec();
            probe.extend_from_slice(buf);
            if !self.refused && probe.windows(self.needle.len()).any(|w| w == self.needle) {
                self.refused = true;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Keyboard progressive enhancement not implemented for the legacy Windows API.",
                ));
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// protects: phimint died at startup on Windows ("Error: Keyboard
    /// progressive enhancement not implemented for the legacy Windows API.")
    /// because the kitty push is a progressive enhancement that the legacy
    /// Windows console API cannot express, and its error was fatal.
    /// fails_when: `enter_terminal_mode` propagates a refused keyboard push.
    #[test]
    fn a_refused_keyboard_push_does_not_abort_tui_startup() {
        let mut out = RefuseOn {
            written: Vec::new(),
            needle: b"\x1B[>5u", // push: CSI > {DISAMBIGUATE|REPORT_ALTERNATE_KEYS} u
            refused: false,
        };
        enter_terminal_mode(&mut out).expect("the keyboard push is optional; setup must continue");
        assert!(
            out.written
                .windows(b"\x1B[?1049h".len())
                .any(|w| w == b"\x1B[?1049h"),
            "the alternate screen must still be entered: {:?}",
            String::from_utf8_lossy(&out.written)
        );
    }

    /// protects: the same refusal at teardown used to eat every command behind
    /// it in the `execute!` chain — on Windows `LeaveAlternateScreen` never
    /// ran and the user was stranded on the blank alternate screen after exit.
    /// fails_when: `restore_terminal_mode` sends its steps as one chained
    /// `execute!`.
    #[test]
    fn a_refused_keyboard_pop_still_leaves_the_alternate_screen() {
        let mut out = RefuseOn {
            written: Vec::new(),
            needle: b"\x1B[<1u", // pop: CSI < 1 u
            refused: false,
        };
        restore_terminal_mode(&mut out);
        assert!(out.refused, "the pop must be attempted");
        assert!(
            out.written
                .windows(b"\x1B[?1049l".len())
                .any(|w| w == b"\x1B[?1049l"),
            "LeaveAlternateScreen must survive a refused pop: {:?}",
            String::from_utf8_lossy(&out.written)
        );
    }
}
