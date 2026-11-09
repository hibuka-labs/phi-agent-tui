//! Tests for the run loop's helpers.

#[cfg(test)]
mod resume_digest_tests {
    use super::super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(text)
    }

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some(text.into()),
            reasoning_content: None,
            thinking_signature: None,
            tool_calls: None,
        }
    }

    #[test]
    fn digest_counts_turns_and_messages_with_title() {
        let msgs = vec![
            user("你好"),
            assistant("你好！"),
            ChatMessage::Tool {
                tool_call_id: "tc1".into(),
                name: Some("read".into()),
                content: "x".into(),
                images: Vec::new(),
            },
            user("继续"),
            assistant("好的"),
        ];
        let line = resume_digest_line(&[], &msgs, Some("探究工程".into()));
        assert!(line.contains("resumed session \"探究工程\""), "{line}");
        assert!(line.contains("2 turns"), "{line}");
        assert!(line.contains("5 messages"), "{line}");
        assert!(line.contains("PgUp"), "{line}");
    }

    #[test]
    fn digest_omits_title_when_absent() {
        let msgs = vec![user("hi"), assistant("hello")];
        let line = resume_digest_line(&[], &msgs, None);
        assert!(!line.contains('"'), "{line}");
        assert!(line.contains("1 turn, 2 messages"), "{line}");
    }

    /// Ephemeral user messages (skill bodies) never reach the persisted
    /// history, but if one ever did leak into a restored list it must not
    /// inflate the turn count the user is told about.
    #[test]
    fn digest_does_not_count_ephemeral_as_turn() {
        let msgs = vec![ChatMessage::user_ephemeral("skill body"), assistant("ok")];
        let line = resume_digest_line(&[], &msgs, None);
        assert!(line.contains("0 turns, 2 messages"), "{line}");
    }

    /// Terminal.app needs View → Allow Mouse Reporting before wheel events
    /// reach the app; users get an actionable notice instead of a dead
    /// trackpad. Other terminals (iTerm2, Ghostty, ...) are untouched.
    #[test]
    fn scroll_notice_targets_apple_terminal_only() {
        let notice = terminal_scroll_notice(Some("Apple_Terminal")).expect("notice expected");
        assert!(notice.contains("Allow Mouse Reporting"), "{notice}");
        assert!(notice.contains("fn+Up"), "{notice}");
        assert_eq!(terminal_scroll_notice(Some("iTerm.app")), None);
        assert_eq!(terminal_scroll_notice(Some("ghostty")), None);
        assert_eq!(terminal_scroll_notice(None), None);
    }

    /// The exit diagnostic fires only for the full failure fingerprint in
    /// Apple Terminal: zero wheel events all run, and there was more history
    /// than one screen. Any wheel event, another terminal, or a short
    /// transcript keeps the exit output clean.
    #[test]
    fn wheel_dead_notice_needs_apple_terminal_zero_scroll_and_history() {
        let notice =
            wheel_dead_notice(Some("Apple_Terminal"), false, true).expect("notice expected");
        assert!(notice.contains("no wheel events were received"), "{notice}");
        assert!(notice.contains("Allow Mouse Reporting"), "{notice}");
        // Saw a wheel event: reporting works, stay quiet.
        assert_eq!(wheel_dead_notice(Some("Apple_Terminal"), true, true), None);
        // Nothing to scroll: the message would be noise.
        assert_eq!(
            wheel_dead_notice(Some("Apple_Terminal"), false, false),
            None
        );
        // Other terminals handle wheel natively or via their own settings.
        assert_eq!(wheel_dead_notice(Some("iTerm.app"), false, true), None);
        assert_eq!(wheel_dead_notice(None, false, true), None);
    }
}

/// Replay must follow the Normal-prose contract: one raw `OutputLine`, wrapped
/// at display time. Pre-wrapping froze the resume-time width into the
/// transcript — a later resize kept the stale breaks, because
/// `rewrap_output` deliberately leaves Normal lines whole.
#[cfg(test)]
mod replay_tests {
    use super::super::*;
    use crate::app::App;
    use phi_tui::lines::LineKind;

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some(text.into()),
            reasoning_content: None,
            thinking_signature: None,
            tool_calls: None,
        }
    }

    #[test]
    fn replay_keeps_prose_as_one_raw_line() {
        let mut app = App::new();
        let long = "The policy lives in the wake module and batches completions behind a quiet window so a burst of finished tasks costs one notification turn, not ten of them.";
        let msgs = vec![assistant(long)];
        replay_messages_to_transcript(&mut app, &[], &msgs);
        let normals: Vec<_> = app
            .transcript
            .output
            .iter()
            .filter(|l| l.kind == LineKind::Normal)
            .collect();
        assert_eq!(normals.len(), 1, "prose must stay one logical line");
        assert_eq!(normals[0].text, long);
        assert_eq!(normals[0].original.as_deref(), Some(long));
    }

    /// The tool preview used to be a raw byte slice at 120, which panics
    /// mid-UTF-8 on non-ASCII output (any CJK repo) during resume.
    #[test]
    fn replay_tool_preview_never_panics_on_multibyte() {
        let mut app = App::new();
        let msgs = vec![ChatMessage::Tool {
            tool_call_id: "tc1".into(),
            name: Some("bash".into()),
            content: "中".repeat(200), // 600 bytes — past the old cut
            images: Vec::new(),
        }];
        replay_messages_to_transcript(&mut app, &[], &msgs);
        let preview = app
            .transcript
            .output
            .iter()
            .find(|l| l.kind == LineKind::ToolResult)
            .expect("tool result line");
        assert!(preview.text.contains("bash"), "{}", preview.text);
        assert!(preview.text.len() < 300, "preview elided: {}", preview.text);
    }

    /// A rotated session splits its history across the archive and the live
    /// window. Resume used to replay only the live window, so the past was
    /// invisible — which from the outside is "resume shows no history".
    #[test]
    fn replay_shows_archived_windows_before_the_live_window() {
        let mut app = App::new();
        let archived = vec![ChatMessage::user("from the archived window")];
        let current = vec![assistant("from the live window")];
        replay_messages_to_transcript(&mut app, &archived, &current);

        let text: Vec<&str> = app
            .transcript
            .output
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        let archived_at = text
            .iter()
            .position(|t| t.contains("from the archived window"))
            .expect("archived line");
        let current_at = text
            .iter()
            .position(|t| t.contains("from the live window"))
            .expect("live line");
        assert!(
            archived_at < current_at,
            "archive is older, so it must render first"
        );
        assert!(
            text.iter().any(|t| t.contains("current window")),
            "a divider must separate the two when both are present: {text:?}"
        );
    }

    #[test]
    fn replay_omits_divider_when_only_one_source_has_content() {
        let mut app = App::new();
        replay_messages_to_transcript(&mut app, &[], &[assistant("only the live window")]);
        let text: Vec<&str> = app
            .transcript
            .output
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        assert!(
            !text.iter().any(|t| t.contains("current window")),
            "{text:?}"
        );
    }

    #[test]
    fn resume_digest_counts_archived_and_live_messages() {
        let archived = vec![
            ChatMessage::user("older question"),
            assistant("older answer"),
        ];
        let current = vec![ChatMessage::user("latest question")];
        let line = resume_digest_line(&archived, &current, None);
        assert!(line.contains("2 turns"), "{line}");
        assert!(line.contains("3 messages"), "{line}");
    }
}

#[cfg(test)]
mod session_slot_tests {
    use super::super::*;
    use crate::shell::SessionIo;
    use std::sync::Arc;

    fn session_io(help: &str) -> SessionIo {
        SessionIo {
            enumerate: Arc::new(|_, _| Vec::new()),
            archived: Arc::new(|_, _| Vec::new()),
            restore: Arc::new(|_, _, _| true),
            resume_help: help.to_string(),
        }
    }

    #[test]
    fn resume_row_follows_the_session_slot() {
        // O6 (T13): 不填即关 — no session slot, no /resume row; the slot's
        // help text is product copy that rides the row.
        assert!(resume_row(&None).is_none());
        let row = resume_row(&Some(session_io("Switch to another session")));
        assert_eq!(
            row,
            Some((
                "resume".to_string(),
                "Switch to another session".to_string()
            ))
        );
    }
}

#[cfg(test)]
mod turn_end_event_tests {
    use super::super::*;
    use phi_agent::AgentError;

    /// A user cancel is not an error and never a plain done: both engine
    /// cancel shapes settle as `TurnCancelled`, stamping the verdict with the
    /// settle itself — the `RunCancelled` bus hop is not load-bearing for the
    /// summary word.
    #[test]
    fn a_user_cancel_settles_as_cancelled_not_an_error() {
        let err: AgentResult<RunOutcome> = Err(AgentError::Cancelled);
        assert!(matches!(turn_end_event(err), TuiEvent::TurnCancelled));
        assert!(matches!(
            turn_end_event(Ok(RunOutcome::Cancelled)),
            TuiEvent::TurnCancelled
        ));
    }

    #[test]
    fn real_failures_stay_errors() {
        assert!(matches!(
            turn_end_event(Ok(RunOutcome::Failed { error: "boom".into() })),
            TuiEvent::TurnError(e) if e == "boom"
        ));
        assert!(matches!(
            turn_end_event(Ok(RunOutcome::MaxTurnsExceeded { turns: 7 })),
            TuiEvent::TurnError(e) if e == "max turns (7) exceeded"
        ));
        let err: AgentResult<RunOutcome> = Err(AgentError::Internal("io broke".into()));
        assert!(matches!(
            turn_end_event(err),
            TuiEvent::TurnError(e) if e.contains("io broke")
        ));
    }

    #[test]
    fn completed_and_continuing_settle_as_done() {
        assert!(matches!(
            turn_end_event(Ok(RunOutcome::Completed)),
            TuiEvent::TurnDone
        ));
        assert!(matches!(
            turn_end_event(Ok(RunOutcome::Continuing)),
            TuiEvent::TurnDone
        ));
    }
}
