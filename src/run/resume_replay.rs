//! Resume history replay and the resume digest line.

use super::*;

/// One-line digest pushed after the replayed history. The resume viewport
/// lands on the tail of the conversation, so without this the user has no
/// signal that the full history is loaded (or that PgUp reaches it).
/// Digest of what a resume restored: archived windows + the live window.
///
/// Both slices count — a rotated session's history is split across them, and
/// a digest that only mentioned the live window would under-report the
/// conversation the user just scrolled into.
pub(super) fn resume_digest_line(
    archived: &[ChatMessage],
    current: &[ChatMessage],
    title: Option<String>,
) -> String {
    let messages: Vec<&ChatMessage> = archived.iter().chain(current.iter()).collect();
    let turns = messages
        .iter()
        .filter(|m| {
            matches!(
                m,
                ChatMessage::User {
                    ephemeral: false,
                    ..
                }
            )
        })
        .count();
    // Leading space so a titled digest reads `resumed session "foo"` and an
    // untitled one still reads `resumed session` with no dangling gap.
    let label = title.map(|t| format!(" \"{t}\"")).unwrap_or_default();
    let turn_word = if turns == 1 { "turn" } else { "turns" };
    let msg_word = if messages.len() == 1 {
        "message"
    } else {
        "messages"
    };
    format!(
        "↩ resumed session{label}: {turns} {turn_word}, {} {msg_word}. PgUp scrolls history, send a message to continue",
        messages.len()
    )
}

/// Drop the live-window head that already appears in the archive.
///
/// A restored window (or a rotation's user trail) sits in both slices — show
/// it once, from the archive, which is the fuller copy. Pure display de-dup
/// (the model still resumes from the live window as-is).
pub(super) fn drop_already_shown<'a>(
    archived: &[ChatMessage],
    live: &'a [ChatMessage],
) -> &'a [ChatMessage] {
    let seen: std::collections::HashSet<_> = archived.iter().filter_map(replay_key).collect();
    let mut start = 0;
    for msg in live {
        match replay_key(msg) {
            Some(key) if seen.contains(&key) => start += 1,
            _ => break,
        }
    }
    &live[start..]
}

/// Identity of a message for replay de-duplication.
pub(super) fn replay_key(msg: &ChatMessage) -> Option<(u8, String, String)> {
    match msg {
        ChatMessage::User { content, .. } => Some((0, String::new(), content.clone())),
        ChatMessage::Assistant {
            content: Some(c), ..
        } if !c.is_empty() => Some((1, String::new(), c.clone())),
        ChatMessage::Tool { name, content, .. } => {
            Some((2, name.clone().unwrap_or_default(), content.clone()))
        }
        _ => None,
    }
}

/// Replay historical `ChatMessage`s into the TUI transcript so the user sees
/// the previous conversation after `--resume`.
///
/// `archived` are the rotated-out windows from the history store and
/// `current` is the live window from `messages.jsonl`. Archived first, in
/// chronological order, with a divider between the two only when both are
/// present.
pub(super) fn replay_messages_to_transcript(
    app: &mut crate::app::App,
    archived: &[ChatMessage],
    current: &[ChatMessage],
) {
    if archived.is_empty() && current.is_empty() {
        return;
    }

    app.push_system("── resumed conversation ──");

    for msg in archived {
        push_replayed_message(app, msg);
    }
    if !archived.is_empty() && !current.is_empty() {
        app.push_system("── current window ──");
    }
    for msg in current {
        push_replayed_message(app, msg);
    }

    app.push_system("── end of resumed conversation ──");
}

/// Push one historical message into the transcript as display lines.
pub(super) fn push_replayed_message(app: &mut crate::app::App, msg: &ChatMessage) {
    use phi_tui::lines::{LineKind, OutputLine};

    match msg {
        ChatMessage::User { content, .. } => {
            app.transcript.push_user(content);
        }
        ChatMessage::Assistant {
            content,
            tool_calls,
            ..
        } => {
            // Push text content (if any).
            // ONE raw OutputLine for the whole message: Normal prose is
            // wrapped at display time (markdown + `wrap_line`), and
            // `rewrap_output` keeps Normal lines whole for exactly that
            // reason. Pre-wrapping here froze the resume-time width into
            // the transcript, so a resized terminal kept stale breaks.
            if let Some(text) = content
                && !text.is_empty()
            {
                let text = text.clone();
                app.transcript.push(OutputLine {
                    original: Some(text.clone()),
                    text,
                    kind: LineKind::Normal,
                    spans: None,
                    detail: None,
                    tool_state: None,
                });
            }
            // Push tool call markers.
            if let Some(tcs) = tool_calls {
                for tc in tcs {
                    app.transcript.push(OutputLine {
                        text: format!("⚙ {}", tc.name),
                        kind: LineKind::Tool,
                        spans: None,
                        original: None,
                        detail: None,
                        tool_state: None,
                    });
                }
            }
        }
        ChatMessage::Tool { name, content, .. } => {
            let label = name.as_deref().unwrap_or("tool");
            let preview = content.lines().next().unwrap_or(content);
            // `elide`, not a byte slice: `&preview[..120]` panics mid-UTF-8
            // on non-ASCII tool output (any CJK repo), and the budget is
            // display columns so the row cannot overflow the pane.
            let display = format!(
                "↻ {}: {}",
                label,
                phi_tui::wrap::elide(preview, 120, phi_tui::wrap::Elide::Head)
            );
            app.transcript.push(OutputLine {
                text: display,
                kind: LineKind::ToolResult,
                spans: None,
                original: None,
                detail: None,
                tool_state: None,
            });
        }
        _ => {} // System / Custom — machine context, not conversation; skip.
    }
}
