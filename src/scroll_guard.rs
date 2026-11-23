//! Scroll-companion arrow suppression (FinalShell dual-stream guard).
//!
//! Some terminals (FinalShell on macOS) translate one wheel gesture into
//! BOTH a real wheel event and synthesized ↑/↓ key events. The wheel half
//! is wanted (the viewport scrolls); the arrow half flips input history
//! behind the user's back. The two streams are indistinguishable per key —
//! the synthesized arrows are byte-identical to typed ones — so the guard
//! reasons by context instead: a bare ↑/↓ that rides in on wheel activity
//! (same drained batch, or within [`SCROLL_TAIL`] of the last wheel event)
//! is treated as synthesized and dropped. Alt+↑/↓ keep their cursor
//! semantics and are never touched.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};
use std::time::{Duration, Instant};

/// How long after the last wheel event a bare ↑/↓ still counts as a
/// companion. Covers the momentum tail: the finger lifts, but the
/// terminal keeps emitting wheel+arrow pairs for a few hundred ms.
pub(crate) const SCROLL_TAIL: Duration = Duration::from_millis(300);

/// Decides whether a key event is a terminal-synthesized scroll companion.
///
/// Off (`enabled = false`) is a full no-op — products that never opted in
/// see zero behavior change.
#[derive(Default)]
pub(crate) struct ScrollArrowGuard {
    enabled: bool,
    /// When wheel activity was last seen (batch pre-scan or earlier batch).
    last_wheel: Option<Instant>,
}

impl ScrollArrowGuard {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            last_wheel: None,
        }
    }

    /// Record wheel activity from a whole drained batch — called BEFORE any
    /// key in the batch is dispatched, which is what makes the decision
    /// order-independent (the terminal may lead with either stream).
    pub(crate) fn note_batch(&mut self, batch: &[Event], now: Instant) {
        if !self.enabled {
            return;
        }
        if batch.iter().any(is_wheel) {
            self.last_wheel = Some(now);
        }
    }

    /// True when this key must be dropped as a synthesized scroll companion.
    /// Only bare ↑/↓ qualify: Alt+↑/↓ keep cursor movement, and no other
    /// key is ever affected.
    pub(crate) fn suppress(&self, code: KeyCode, modifiers: KeyModifiers, now: Instant) -> bool {
        if !self.enabled {
            return false;
        }
        if !matches!(code, KeyCode::Up | KeyCode::Down) {
            return false;
        }
        if !modifiers.is_empty() {
            return false;
        }
        self.last_wheel
            .is_some_and(|t| now.saturating_duration_since(t) <= SCROLL_TAIL)
    }
}

/// Wheel events the guard listens for: vertical wheel only. Horizontal
/// scroll translates to ←/→ at worst, which never enters history.
fn is_wheel(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, MouseEvent};

    fn wheel() -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// protects: a bare ↑ in a wheel+arrow batch is dropped whichever stream
    /// leads — the dual stream's arrow half must never flip history.
    /// fails_when: only the wheel-first order is covered. no_claim: the
    /// scroll itself (the wheel half is dispatched to the viewport).
    #[test]
    fn the_wheel_and_arrow_batch_drops_the_arrow() {
        let mut g = ScrollArrowGuard::new(true);
        let t0 = Instant::now();
        g.note_batch(&[wheel(), key(KeyCode::Up)], t0);
        assert!(g.suppress(KeyCode::Up, KeyModifiers::NONE, t0));
        assert!(g.suppress(KeyCode::Down, KeyModifiers::NONE, t0));
    }

    /// protects: the same batch with the arrow FIRST is also covered — the
    /// pre-scan runs before dispatch, so in-batch order is irrelevant.
    /// fails_when: the guard only remembers wheels it already dispatched.
    /// no_claim: how the terminal orders its two streams.
    #[test]
    fn the_arrow_before_the_wheel_in_one_batch_is_dropped_too() {
        let mut g = ScrollArrowGuard::new(true);
        let t0 = Instant::now();
        g.note_batch(&[key(KeyCode::Up), wheel()], t0);
        assert!(g.suppress(KeyCode::Up, KeyModifiers::NONE, t0));
    }

    /// protects: the momentum tail — arrows arriving in later batches still
    /// ride the wheel gesture and are dropped within SCROLL_TAIL.
    /// fails_when: suppression ends with the wheel's own batch.
    /// no_claim: arrows typed deliberately long after scrolling stopped.
    #[test]
    fn the_momentum_tail_drops_arrows_after_the_wheel_batch() {
        let mut g = ScrollArrowGuard::new(true);
        let t0 = Instant::now();
        g.note_batch(&[wheel()], t0);
        assert!(g.suppress(
            KeyCode::Up,
            KeyModifiers::NONE,
            t0 + SCROLL_TAIL - Duration::from_millis(1)
        ));
        assert!(!g.suppress(
            KeyCode::Up,
            KeyModifiers::NONE,
            t0 + SCROLL_TAIL + Duration::from_millis(1)
        ));
    }

    /// protects: with no recent wheel, a bare ↑ keeps its history-recall
    /// job — the guard is context-triggered, never a key remap.
    /// fails_when: arrows are suppressed unconditionally.
    /// no_claim: what recall does once the key passes.
    #[test]
    fn a_bare_arrow_with_no_recent_wheel_passes() {
        let g = ScrollArrowGuard::new(true);
        let t0 = Instant::now();
        assert!(!g.suppress(KeyCode::Up, KeyModifiers::NONE, t0));
    }

    /// protects: a pure-key batch arms nothing — keys alone never count as
    /// wheel activity, however many of them arrive together.
    /// fails_when: any batch arms the tail. no_claim: mixed batches.
    #[test]
    fn a_key_only_batch_arms_nothing() {
        let mut g = ScrollArrowGuard::new(true);
        let t0 = Instant::now();
        g.note_batch(&[key(KeyCode::Char('a')), key(KeyCode::Up)], t0);
        assert!(!g.suppress(KeyCode::Up, KeyModifiers::NONE, t0));
    }

    /// protects: Alt+↑/↓ keep pure cursor movement even inside the window —
    /// the synthesized stream is always bare, so chords are never dropped.
    /// fails_when: modifier arrows are swept up with the bare ones.
    /// no_claim: what the chord does at the composer.
    #[test]
    fn alt_arrows_keep_their_cursor_meaning() {
        let mut g = ScrollArrowGuard::new(true);
        let t0 = Instant::now();
        g.note_batch(&[wheel()], t0);
        assert!(!g.suppress(KeyCode::Up, KeyModifiers::ALT, t0));
        assert!(!g.suppress(KeyCode::Down, KeyModifiers::ALT, t0));
    }

    /// protects: no other key is affected — typing through a scroll burst
    /// (IME, fast keys) still reaches the composer.
    /// fails_when: suppression spreads past bare ↑/↓.
    /// no_claim: the composer's own handling of those keys.
    #[test]
    fn other_keys_are_never_dropped() {
        let mut g = ScrollArrowGuard::new(true);
        let t0 = Instant::now();
        g.note_batch(&[wheel()], t0);
        assert!(!g.suppress(KeyCode::Char('a'), KeyModifiers::NONE, t0));
        assert!(!g.suppress(KeyCode::Left, KeyModifiers::NONE, t0));
        assert!(!g.suppress(KeyCode::Enter, KeyModifiers::NONE, t0));
    }

    /// protects: the off switch is a full no-op — products that never opted
    /// in pass every key, dual stream or not.
    /// fails_when: the guard drops arrows regardless of `enabled`.
    /// no_claim: what an opted-in product does.
    #[test]
    fn the_switch_off_passes_everything() {
        let mut g = ScrollArrowGuard::new(false);
        let t0 = Instant::now();
        g.note_batch(&[wheel(), key(KeyCode::Up)], t0);
        assert!(!g.suppress(KeyCode::Up, KeyModifiers::NONE, t0));
    }

    /// protects: the drain loop's dispatch shape — note the whole batch
    /// first, then walk it; exactly the bare arrows are skipped and nothing
    /// else. This is the wiring contract `run/input.rs` relies on.
    /// fails_when: a skipped key is not an arrow, or a companion arrow
    /// reaches `handle_key_event`. no_claim: the per-event side effects.
    #[test]
    fn the_drain_shape_skips_only_the_companion_arrows() {
        let mut g = ScrollArrowGuard::new(true);
        let t0 = Instant::now();
        let batch = vec![
            key(KeyCode::Up),
            wheel(),
            key(KeyCode::Char('x')),
            key(KeyCode::Down),
            key(KeyCode::Up), // second arrow: momentum tail inside one batch
        ];
        g.note_batch(&batch, t0);
        let dispatched: Vec<KeyCode> = batch
            .iter()
            .filter_map(|e| match e {
                Event::Key(k) if !g.suppress(k.code, k.modifiers, t0) => Some(k.code),
                _ => None,
            })
            .collect();
        assert_eq!(dispatched, vec![KeyCode::Char('x')]);
    }
}
