//! The approval mode vocabulary: what the next tool call does, and the shared
//! live switch between the TUI (Shift+Tab) and the product's approval gate.
//!
//! Mechanism half of the approval mechanism/policy split (design §5.1,
//! discovery 6): the mode switch and its status-bar badge live here because the
//! shell flips and renders them; the grading tables, risk copy and the ledger
//! (`classify_command`, `SAFE_PREFIXES`, `LiveApprovalGate`, …) stay product —
//! a product decides *what* needs approval, the shell only carries *how the
//! user steers the ask level*.

use std::sync::atomic::{AtomicU8, Ordering};

// The approval queue item is engine vocabulary — re-exported so shell call
// sites name one module for the whole interaction surface.
pub use phi_agent::ApprovalItem;

/// Runtime approval mode — what the next tool call does.
///
/// The UI (Shift+Tab) flips it through [`ApprovalModeSwitch`]; the gate reads
/// it on every tool call, so a flip takes effect immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RuntimeApprovalMode {
    /// Every tool call auto-approves (the policy short-circuits to `None`).
    Auto = 0,
    /// Writes and risky shell prompt; reads pass. The interactive default.
    Ask = 1,
    /// Writes and risky shell are rejected. CLI startup mode only — Shift+Tab
    /// leaves it (first press enters `ask`), it is not a cycle stop.
    Deny = 2,
}

impl RuntimeApprovalMode {
    /// Parse a CLI `--approval` value. Unknown values fall back to `auto`
    /// (matches the historical `build_approval` fallback).
    pub fn parse(s: &str) -> Self {
        match s {
            "ask" => Self::Ask,
            "deny" => Self::Deny,
            _ => Self::Auto,
        }
    }

    // The status-bar badge wording is product copy — `ApprovalCopy::mode_badge`
    // (T14); the severity color is shell mechanism (render::render_status).
}

/// Shared live switch between the TUI (Shift+Tab) and the approval gate.
///
/// One `Arc` handed to both sides: the UI flips it, the gate reads it per
/// tool call. No locks in the hot path (`AtomicU8`), no channel — the mode is
/// a single byte of shared state.
#[derive(Debug)]
pub struct ApprovalModeSwitch {
    mode: AtomicU8,
}

impl ApprovalModeSwitch {
    pub fn new(initial: RuntimeApprovalMode) -> Self {
        Self {
            mode: AtomicU8::new(initial as u8),
        }
    }

    pub fn get(&self) -> RuntimeApprovalMode {
        match self.mode.load(Ordering::SeqCst) {
            1 => RuntimeApprovalMode::Ask,
            2 => RuntimeApprovalMode::Deny,
            _ => RuntimeApprovalMode::Auto,
        }
    }

    pub fn set(&self, mode: RuntimeApprovalMode) {
        self.mode.store(mode as u8, Ordering::SeqCst);
    }

    /// Shift+Tab cycle: `auto` ⇄ `ask`. `deny` is a CLI startup mode, not a
    /// cycle stop — the first press leaves it for `ask`.
    pub fn cycle_auto_ask(&self) -> RuntimeApprovalMode {
        let next = match self.get() {
            RuntimeApprovalMode::Auto => RuntimeApprovalMode::Ask,
            RuntimeApprovalMode::Ask => RuntimeApprovalMode::Auto,
            RuntimeApprovalMode::Deny => RuntimeApprovalMode::Ask,
        };
        self.set(next);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cycle_leaves_the_startup_brake() {
        let switch = ApprovalModeSwitch::new(RuntimeApprovalMode::Deny);
        assert_eq!(switch.cycle_auto_ask(), RuntimeApprovalMode::Ask);
        assert_eq!(switch.cycle_auto_ask(), RuntimeApprovalMode::Auto);
    }

    #[test]
    fn unknown_cli_values_fall_back_to_auto() {
        assert_eq!(
            RuntimeApprovalMode::parse("nope"),
            RuntimeApprovalMode::Auto
        );
        assert_eq!(RuntimeApprovalMode::parse("ask"), RuntimeApprovalMode::Ask);
    }
}
