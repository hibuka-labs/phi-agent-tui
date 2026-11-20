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

/// The panic hook's restore — what runs on the panicking thread before the
/// default hook prints the panic. It is the ONLY restore that runs when a
/// panic aborts (no unwind ever reaches [`TerminalGuard`]), so it must undo
/// everything the TUI armed via [`enter_terminal_mode`] — mouse reporting
/// included, or the emulator keeps emitting `CSI < Cb;Cx;Cy M` report
/// fragments into whatever shell inherits the terminal (observed 2026-11-17).
pub(super) fn restore_after_panic(out: &mut impl io::Write) {
    let _ = disable_raw_mode();
    restore_terminal_mode(out);
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
        // The pop must be attempted — but only where crossterm expresses it
        // as an escape sequence. On Windows the legacy console API refuses
        // the pop outright ("Keyboard progressive enhancement not
        // implemented for the legacy Windows API.") without ever writing it
        // to the writer, so the mock cannot see the attempt there. The
        // refusal is real on both platforms; only its delivery differs.
        #[cfg(not(windows))]
        assert!(out.refused, "the pop must be attempted");
        // The alternate screen must be left whatever the pop did — this is
        // the invariant the refusal actually broke: the pop used to eat the
        // LeaveAlternateScreen behind it and strand the user on the blank
        // alternate screen after exit.
        assert!(
            out.written
                .windows(b"\x1B[?1049l".len())
                .any(|w| w == b"\x1B[?1049l"),
            "LeaveAlternateScreen must survive a refused pop: {:?}",
            String::from_utf8_lossy(&out.written)
        );
    }

    /// The Windows twin of the test above: here the refusal arrives from the
    /// legacy console API, not from the writer. crossterm's
    /// `is_ansi_code_supported()` is false for the kitty keyboard pop, so
    /// `queue()` takes the WinAPI branch and answers with "Keyboard
    /// progressive enhancement not implemented for the legacy Windows API."
    /// *without* writing the escape sequence — the mock never sees a
    /// refusal, because the refusal happened off-writer.
    ///
    /// What must still hold is the same invariant: the alternate screen is
    /// left behind the refused pop.
    #[cfg(windows)]
    #[test]
    fn on_windows_the_pop_is_refused_by_the_console_api_and_never_written() {
        let mut out = RefuseOn {
            written: Vec::new(),
            needle: b"\x1B[<1u", // pop: CSI < 1 u
            refused: false,
        };
        restore_terminal_mode(&mut out);
        assert!(!out.refused, "the WinAPI refusal never reaches the writer");
        assert!(
            !out.written
                .windows(b"\x1B[<1u".len())
                .any(|w| w == b"\x1B[<1u"),
            "the refused pop must not be written: {:?}",
            String::from_utf8_lossy(&out.written)
        );
        assert!(
            out.written
                .windows(b"\x1B[?1049l".len())
                .any(|w| w == b"\x1B[?1049l"),
            "LeaveAlternateScreen must survive the refused pop: {:?}",
            String::from_utf8_lossy(&out.written)
        );
    }

    /// protects: a panic that aborts never unwinds to `TerminalGuard`, so the
    /// panic hook is the only restore that runs — and it used to skip
    /// `DisableMouseCapture`, leaving mouse reporting armed in the emulator.
    /// The shell that inherited the terminal then got flooded with
    /// `CSI < Cb;Cx;Cy M` mouse-report fragments on every pointer move (live:
    /// 2026-11-17, residue at the local prompt after the remote session died).
    /// fails_when: `restore_after_panic` omits the mouse resets
    /// (`?1006l`/`?1003l`/`?1000l`) or sends its steps as one chained
    /// `execute!` that a refused kitty pop can eat.
    #[cfg(not(windows))] // Windows disarms mouse via the console API, off-writer
    #[test]
    fn the_panic_restore_disables_mouse_reporting() {
        let mut out = RefuseOn {
            written: Vec::new(),
            needle: b"\x1B[<1u", // pop: CSI < 1 u
            refused: false,
        };
        restore_after_panic(&mut out);
        let dump = String::from_utf8_lossy(&out.written);
        for seq in ["\x1B[?1006l", "\x1B[?1003l", "\x1B[?1000l", "\x1B[?1049l"] {
            assert!(
                out.written.windows(seq.len()).any(|w| w == seq.as_bytes()),
                "{seq} must be sent by the panic restore: {dump:?}"
            );
        }
    }

    /// The Windows twin: the console API disarms mouse reporting off-writer
    /// (like the kitty pop), so no mouse bytes ever reach the mock — what must
    /// still hold is that the panic restore leaves the alternate screen.
    #[cfg(windows)]
    #[test]
    fn on_windows_the_panic_restore_still_leaves_the_alternate_screen() {
        let mut out = RefuseOn {
            written: Vec::new(),
            needle: b"\x1B[<1u", // pop: CSI < 1 u
            refused: false,
        };
        restore_after_panic(&mut out);
        assert!(
            out.written
                .windows(b"\x1B[?1049l".len())
                .any(|w| w == b"\x1B[?1049l"),
            "LeaveAlternateScreen must be sent by the panic restore: {:?}",
            String::from_utf8_lossy(&out.written)
        );
    }
}
