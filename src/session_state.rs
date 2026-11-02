//! Session state management for the TUI.
//!
//! [`CurrentSession`] bundles all session-related state so that `/resume`
//! (or any future session-switching command) can swap sessions by replacing
//! a single value.

use phi_agent::{ChatMessage, SessionContext, SessionId};

/// The active session state for the TUI.
///
/// Holds everything that changes when the user switches sessions. Created once
/// at startup; replaced wholesale on `/resume`.
pub struct CurrentSession {
    /// Agent-base runtime session ID.
    pub session_id: SessionId,
    /// phi-agent session context (directory, lock, log path).
    pub ctx: SessionContext,
    /// Transcript rows replayed at boot from a resumed session's messages
    /// (`None` for a fresh session). Set again on each `/resume` switch.
    pub resume_messages: Option<Vec<ChatMessage>>,
}

impl CurrentSession {
    /// Shortcut to the session directory.
    pub fn session_dir(&self) -> &std::path::Path {
        &self.ctx.session_dir
    }
}
