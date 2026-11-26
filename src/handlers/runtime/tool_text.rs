//! Tool-call text projection: invocation labels, result labels, and the
//! structured block a call attaches under its invocation row.

use phi_tui::diff::{diff_to_hunks, diff_to_hunks_indexed};
use phi_tui::lines::{DiffHunk, LineDetail, LineKind, MetaHead, OutputLine, ToolState};
use phi_tui::wrap::{Elide, elide, one_line};
use unicode_width::UnicodeWidthStr;

use crate::theme::ShellToken;

/// Render a notice line for the TUI.
///
/// Product-layer copy table (design §8): the mechanism layer emits
/// cause-specific English facts ("guard judge unparsed/call failed/unavailable
/// — treating as complete"), but end users have never heard of a "judge" and
/// cannot act on the cause — the three collapse to one user-facing line. The
/// cause stays in `session.log` for debugging. Everything else renders as
/// `source: text`, unless `text` already names the source (no doubling).
pub(super) fn notice_line(source: &str, text: &str) -> String {
    if source == "guard" && text.starts_with("guard judge") {
        return "Answer accepted without verification.".to_string();
    }
    if text.starts_with(source) {
        text.to_string()
    } else {
        format!("{source}: {text}")
    }
}

/// Build the structured block a tool attaches under its invocation line, or
/// `None` when the call has no payload worth a block.
///
/// Four shapes, and the split is deliberate (design §write):
/// - `edit_file` → [`LineDetail::Diff`] — an edit is a *transformation*, and
///   its evidence is never previewed away. Rendered in full, outside the
///   preview ladder.
/// - `write_file` → [`LineDetail::Folded`] holding the content being written.
///   A create is an *artifact*, not a transformation: the useful facts are
///   "what shape, how big", so it rides the preview ladder. If the call turns
///   out to be an overwrite, [`crate::app::App::handle_runtime`] swaps this for a
///   real old→new diff — an overwrite *is* a transformation.
/// - `spawn_agent` → [`LineDetail::Folded`] holding the task brief. Also an
///   artifact (a briefing, not a change): the row keeps a gist of the head,
///   the brief rides the ladder, whole.
/// - `execute_command` → [`LineDetail::Folded`] holding the command, but only
///   when the row could not show it whole (`body` holds the flattened command
///   as the row rendered it; a command that fit needs no block). The row keeps
///   its one-line contract, the elided tail comes back at full expansion.
pub(super) fn build_tool_detail(
    tool_name: &str,
    args_json: &str,
    body: &str,
) -> Option<LineDetail> {
    let args: serde_json::Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(tool = tool_name, error = %e, "build_tool_detail: failed to parse args_json");
            return None;
        }
    };
    // `spawn_agent` → the task brief. An artifact like written content: what
    // matters at a glance is "who, roughly what", so the invocation row keeps
    // the gist and the brief rides the preview ladder, whole.
    if tool_name == "spawn_agent" {
        let task = args.get("task").and_then(|v| v.as_str())?;
        if task.is_empty() {
            return None;
        }
        let (_, meta_head) = spawn_task_head(task);
        return Some(LineDetail::Folded {
            raw: task.to_string(),
            line_count: task.lines().count(),
            char_count: task.chars().count(),
            duration_secs: None,
            meta_head,
        });
    }
    // `execute_command` → the command pays back its elided tail. The row
    // renders the command flattened and head-elided to the row budget; the
    // fold marks that head *abbreviated* so full expansion restores the whole
    // line. When the row already holds the command whole there is nothing to
    // pay back and the block would only echo the row.
    if tool_name == "execute_command" {
        let command = args.get("command").and_then(|v| v.as_str())?;
        let flat = command.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.is_empty() || body.contains(&flat) {
            return None;
        }
        return Some(LineDetail::Folded {
            raw: flat.clone(),
            line_count: 1,
            char_count: flat.chars().count(),
            duration_secs: None,
            meta_head: MetaHead::Abbreviated,
        });
    }
    let path = {
        let p = args.get("path").and_then(|v| v.as_str())?;
        p.to_string()
    };

    match tool_name {
        "edit_file" => {
            let edits = args.get("edits")?.as_array()?;
            let mut all_hunks: Vec<DiffHunk> = Vec::new();
            for (i, edit) in edits.iter().enumerate() {
                let old_text = edit.get("old_text")?.as_str()?;
                let new_text = edit.get("new_text")?.as_str()?;
                all_hunks.extend(diff_to_hunks_indexed(old_text, new_text, 3, i));
            }
            if all_hunks.is_empty() {
                return None;
            }
            Some(LineDetail::Diff {
                path,
                hunks: all_hunks,
            })
        }
        "write_file" => {
            let content = args.get("content").and_then(|v| v.as_str())?;
            if content.is_empty() {
                return None;
            }
            Some(LineDetail::Folded {
                raw: content.to_string(),
                line_count: content.lines().count(),
                char_count: content.chars().count(),
                duration_secs: None,
                // The meta row is the invocation (`* write_file <path>`); it
                // says nothing about the content, so the body owns every row.
                meta_head: MetaHead::None,
            })
        }
        _ => None,
    }
}

/// Basename of a filesystem path for an invocation line.
///
/// The file name is what identifies the call; the parent directories are the
/// same `/Users/…/projects/-Users-…/memory/` prefix repeated on every row.
/// The full path is still in `session.log` and in the tool result.
fn path_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

/// `(tool, key)` pairs whose schema default is `false` — a flag that only
/// carries information when it is `true`.
const FALSE_DEFAULT_FLAGS: &[(&str, &str)] = &[
    ("execute_command", "background"),
    ("history_list_items", "recent_first"),
    ("history_list_windows", "recent_first"),
    ("list_files", "recursive"),
    ("task_output", "wait"),
    ("write_file", "overwrite"),
];

/// `(tool, key, default)` numeric knobs at their schema-documented default.
/// `execute_command.timeout_ms` is conditional on `background` and lives in
/// [`is_default_noise`].
const DEFAULT_NUMBERS: &[(&str, &str, u64)] = &[
    ("history_list_items", "limit", 50),
    ("history_list_windows", "limit", 20),
    ("history_read_item", "limit_chars", 3_000),
    ("history_read_item", "offset_chars", 0),
    ("history_search_contents", "limit", 10),
    ("list_files", "limit", 500),
    ("notes_list_files", "max_results", 50),
    ("notes_read_file", "start_line", 1),
    ("notes_search_contents", "max_files", 20),
    ("notes_search_contents", "max_matches", 5),
    ("read_file", "limit", 2_000),
    ("search_content", "max_results", 20),
    ("task_output", "timeout_ms", 120_000),
];

/// The schema default for `(tool_name, key)`, when the knob has one.
fn default_number(tool_name: &str, key: &str) -> Option<u64> {
    DEFAULT_NUMBERS
        .iter()
        .find(|(t, k, _)| *t == tool_name && *k == key)
        .map(|&(_, _, d)| d)
}

/// Whether `(key, value)` only spells out the tool's schema default.
///
/// Models routinely pass every knob they see — `{"background": false,
/// "timeout_ms": 120000, "command": "cargo build"}` is one fact wearing two
/// decorations. The call still executes with those values and the raw JSON
/// stays in `session.log`; the projection only decides what the row spends
/// its columns on. "Default" means the one the schema documents (the model's
/// notion of leaving a knob alone), so `working_dir` at the workspace the
/// process runs in counts too. A real choice — `background=true`,
/// `limit=100` — is the call and always stays.
fn is_default_noise(
    tool_name: &str,
    key: &str,
    v: &serde_json::Value,
    background: bool,
    workspace_root: &std::path::Path,
) -> bool {
    match v {
        serde_json::Value::Bool(false) => FALSE_DEFAULT_FLAGS.contains(&(tool_name, key)),
        serde_json::Value::Number(n) => {
            let default = if (tool_name, key) == ("execute_command", "timeout_ms") {
                // Foreground dies at the tool timeout (schema: 120000);
                // background runs until it ends (0 = never kill).
                Some(if background { 0 } else { 120_000 })
            } else {
                default_number(tool_name, key)
            };
            default.is_some_and(|d| n.as_u64() == Some(d))
        }
        serde_json::Value::String(s) => match (tool_name, key) {
            // "Wherever the tool already is": the workspace the process runs
            // in, spelled absolute or as `.`.
            ("execute_command", "working_dir")
            | ("list_files", "path")
            | ("search_content", "path") => {
                let s = s.trim();
                s == "." || std::path::Path::new(s) == workspace_root
            }
            _ => false,
        },
        _ => false,
    }
}

/// Argument values that identify a call, with the JSON syntax dropped.
///
/// `args_json` is a syntax dump — `{"limit": 60, "path": "/Users/…/x.md"}` —
/// whose braces, quoted keys and full paths eat the line budget while adding
/// nothing the tool name doesn't already say. The transcript shows the values
/// (rule 3: tool name + readable arguments); the raw JSON stays in the log. Values that
/// merely spell out a schema default are dropped entirely — see
/// [`is_default_noise`].
///
/// Content payloads (`content`, `old_text`/`new_text`, `edits`, `task`) are
/// never inlined here — they are either multi-KB or already rendered as the
/// tool's structured block.
///
/// `budget` is the display columns the joined label may occupy — the room the
/// terminal actually has after the `* {tool}  ` gutter. Each value elides to
/// the columns still left, so a lone `command` fills the line instead of a
/// fixed char count truncating it while the row is still empty. Eliding
/// flattens whitespace first (`split_whitespace`), matching the old
/// `one_line` single-row contract.
fn args_label(
    tool_name: &str,
    args: &serde_json::Value,
    budget: usize,
    workspace_root: &std::path::Path,
) -> String {
    // Payload keys shown elsewhere (structured block) or too long to inline.
    const SKIP: &[&str] = &[
        "content",
        "old_text",
        "new_text",
        "edits",
        "task",
        "description",
        "prompt",
    ];
    // Path-shaped keys: show the basename only.
    const PATHISH: &[&str] = &["path", "file", "file_path", "dir", "directory", "target"];

    let obj = match args.as_object() {
        Some(o) => o,
        None => return String::new(),
    };

    // read_file's identifying detail is the range, not just the name.
    if tool_name == "read_file" {
        let mut out = match obj.get("path").and_then(|v| v.as_str()) {
            Some(p) => path_label(p),
            None => String::new(),
        };
        let offset = obj.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
        let limit = obj.get("limit").and_then(|v| v.as_u64());
        // The defaults read on from the top of the file; only an explicit
        // window is a fact about this call.
        if offset > 0 {
            out.push_str(&format!(" from L{}", offset + 1));
        }
        if let Some(end) = limit
            .filter(|l| Some(*l) != default_number("read_file", "limit"))
            .map(|l| offset + l)
        {
            out.push_str(&format!(" to L{end}"));
        }
        return out;
    }

    let background = obj
        .get("background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut parts: Vec<String> = Vec::new();
    let mut remaining = budget;
    // `command` leads, bare: it IS the call (every other knob decorates it),
    // and putting it first spends the row's columns on the payload instead of
    // on flags. The rest keeps JSON (alphabetical) order.
    if let Some(serde_json::Value::String(s)) = obj.get("command") {
        let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if !flat.is_empty() {
            let shown = elide(&flat, remaining, Elide::Head);
            remaining = remaining.saturating_sub(shown.width() + 2);
            parts.push(shown);
        }
    }
    for (k, v) in obj {
        if k == "command" {
            continue;
        }
        if SKIP.contains(&k.as_str()) {
            continue;
        }
        if is_default_noise(tool_name, k, v, background, workspace_root) {
            continue;
        }
        if PATHISH.contains(&k.as_str()) {
            if let Some(p) = v.as_str() {
                let label = path_label(p);
                if !label.is_empty() {
                    remaining = remaining.saturating_sub(label.width() + 2);
                    parts.push(label);
                }
            }
            continue;
        }
        match v {
            serde_json::Value::String(s) if s.is_empty() => {}
            serde_json::Value::String(s) => {
                // A command or a pattern is worth showing whole-ish; a bare name
                // reads better without its key. `task_id` is a call's handle
                // (`bg_…`), self-identifying by shape like a basename.
                let bare = k == "name" || k == "pattern" || k == "task_id";
                let kv = if bare { String::new() } else { format!("{k}=") };
                let avail = remaining.saturating_sub(kv.width());
                let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
                let shown = elide(&flat, avail, Elide::Head);
                remaining = remaining.saturating_sub(kv.width() + shown.width() + 2);
                if bare {
                    parts.push(shown);
                } else {
                    parts.push(format!("{kv}{shown}"));
                }
            }
            serde_json::Value::Number(n) => {
                // Durations speak seconds/minutes, not raw milliseconds —
                // `timeout_ms=180000` is machine talk on a human-facing row.
                if let Some(stem) = k.strip_suffix("_ms") {
                    let pretty = n.as_u64().map(fmt_ms).unwrap_or_else(|| n.to_string());
                    parts.push(format!("{stem}={pretty}"));
                } else {
                    parts.push(format!("{k}={n}"));
                }
            }
            // A flag carries information only when set — bare reads as the
            // adjective it is (`background`, `wait`), never `flag=true`.
            serde_json::Value::Bool(true) => parts.push(k.clone()),
            serde_json::Value::Bool(false) => parts.push(format!("{k}=false")),
            serde_json::Value::Null => {}
            // Objects/arrays are structure, not values — they live in the
            // structured block or the log.
            _ => {}
        }
    }
    parts.join("  ")
}

/// Milliseconds in the unit a human would say: `180000` → `3m`, `12000` →
/// `12s`. Sub-second stays raw (`500ms`) — rounding there would lie.
fn fmt_ms(ms: u64) -> String {
    if ms >= 60_000 && ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms >= 1_000 && ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

/// Human label for a tool-result head when the head is a known status
/// envelope — `display projection only`: `raw` keeps the envelope whole (the
/// fold, selection and copy all read `raw`), this only decides what the meta
/// row spells. Unknown heads come back `None` and render as themselves.
pub(super) fn result_head_label(head: &str) -> Option<String> {
    if let Some(rest) = head
        .strip_prefix("[Command Failed (exit code: ")
        .and_then(|r| r.strip_suffix(")]"))
    {
        return Some(format!("failed (exit {rest})"));
    }
    if let Some(rest) = head
        .strip_prefix("[Command Timed Out after ")
        .and_then(|r| r.strip_suffix("ms]"))
    {
        let secs = rest.parse::<u64>().ok().map(fmt_ms)?;
        return Some(format!("timed out ({secs})"));
    }
    match head {
        "[Command Timed Out]" | "[Tool Timeout]" => Some("timed out".to_string()),
        "[Command Terminated]" => Some("terminated".to_string()),
        _ => None,
    }
}

/// Invocation-line body for a tool call: `tool_name` plus readable arguments.
/// No leading glyph — the renderer draws that from the call's [`ToolState`].
///
/// `max_cols` is the terminal width the row may fill; the argument label is
/// budgeted to the columns left after the `* {prefix}{tool_name}  ` gutter so
/// a lone `command` uses the space the row actually has.
pub(super) fn tool_invocation_text(
    tool_name: &str,
    args_json: &str,
    prefix: &str,
    max_cols: usize,
    workspace_root: &std::path::Path,
) -> String {
    if tool_name == "spawn_agent" {
        return spawn_invocation_text(args_json, prefix)
            .unwrap_or_else(|| format!("{prefix}{tool_name}"));
    }
    let args: serde_json::Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(_) => return format!("{prefix}{tool_name}"),
    };
    let gutter = 2 + prefix.width() + tool_name.width() + 2;
    let budget = max_cols.saturating_sub(gutter).max(8);
    let label = args_label(tool_name, &args, budget, workspace_root);
    if label.is_empty() {
        format!("{prefix}{tool_name}")
    } else {
        format!("{prefix}{tool_name}  {label}")
    }
}

/// Budget for the task gist on a `spawn_agent` invocation line, and for the
/// outcome message on its result line.
///
/// **Not** the terminal width: the gist is a label, and a label that
/// re-expands into a paragraph on a wide terminal is the exact dump this line
/// exists to avoid (session 20261006_9264ba3e — four spawn briefs ate the
/// pane). The task brief itself rides the preview ladder, whole, one
/// keypress away.
const SPAWN_GIST_COLS: usize = 56;

/// The task head a spawn invocation line shows, and how it relates to the
/// payload's first line (`MetaHead`), for the fold that carries the brief.
fn spawn_task_head(task: &str) -> (String, MetaHead) {
    let head = task.lines().next().unwrap_or("").trim();
    let gist = one_line(head, SPAWN_GIST_COLS);
    let meta_head = if gist == head {
        MetaHead::Whole
    } else {
        MetaHead::Abbreviated
    };
    (gist, meta_head)
}

/// Compact invocation text for `spawn_agent`: `spawn_agent <name> - <task
/// gist>`. The raw args JSON is a multi-KB dump (the full self-contained
/// task text) — in session 20260904_e6612477 its invisible tail was the dead
/// air of a 20 s LLM call. Name + one task line is all the transcript needs;
/// the fold under the line, the task panel and the tool result carry the rest.
/// `None` → the caller falls back to the generic rendering.
fn spawn_invocation_text(args_json: &str, prefix: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(args_json).ok()?;
    let name = args.get("task_name").and_then(|v| v.as_str())?;
    let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let (gist, _) = spawn_task_head(task);
    Some(if gist.is_empty() {
        format!("{prefix}spawn_agent {name}")
    } else {
        format!("{prefix}spawn_agent {name} - {gist}")
    })
}

/// Human label for a `spawn_agent` result.
///
/// The tool answers with structured JSON (`{"agent_path":…,"message":…}`),
/// and a raw JSON row is a machine dump where the transcript wants a fact:
/// whom was spawned, with what capability. The two facts that must never be
/// silent — a recycled registration slot, a read-only degradation — come
/// from the tool's own `message`; the registered-tool list is a dump and
/// rides the fold with the rest of the raw result.
///
/// `None` when the summary is not that shape → the generic head/meta path.
pub(super) fn spawn_result_label(summary: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(summary).ok()?;
    let path = v.get("agent_path").and_then(serde_json::Value::as_str)?;
    let msg = v
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if path.is_empty() {
        // The spawn failed and the orphan was closed — the message *is* the
        // outcome ("Agent spawned but task delivery failed…").
        return Some(one_line(msg, SPAWN_GIST_COLS));
    }
    let mut facts: Vec<&str> = Vec::new();
    if msg.contains("recycled a finished agent") {
        facts.push("recycled");
    }
    if msg.contains("tools degraded to read-only") {
        facts.push("degraded to read-only");
    } else if let Some(cap) = msg
        .split_once("(tools: ")
        .and_then(|(_, rest)| rest.split([';', ')']).next())
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        facts.push(cap);
    }
    if facts.is_empty() {
        Some(format!("spawned {path}"))
    } else {
        Some(format!("spawned {path} ({})", facts.join(", ")))
    }
}

/// Human label for an `execute_command` background-start result.
///
/// The tool answers with structured JSON
/// (`{"background":true,"task_id":…,"message":…}`), and the fact the row
/// exists for is which task id now owns the command — the task panel and
/// `task_output` are keyed on it. The raw result (with the tool's own
/// `message`) rides the fold.
///
/// `None` when the summary is not that shape → the generic head/meta path.
pub(super) fn bg_start_result_label(summary: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(summary).ok()?;
    if v.get("background").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let task_id = v
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())?;
    Some(format!("background task {task_id} started"))
}

/// Prefix for sub-agent output lines (e.g., `[task_name] `).
pub(crate) fn agent_prefix(agent_id: Option<&str>) -> String {
    match agent_id {
        Some(id) if !id.is_empty() => format!("[{id}] "),
        _ => String::new(),
    }
}

/// The tool name a call row names: the first word after the `* ` marker and
/// any `[agent] ` sub-agent prefix — `* [agent] execute_command  args` and the
/// spawn shorthand `* spawn_agent name - gist` both yield their tool name.
fn row_tool_name(text: &str) -> &str {
    let body = text.strip_prefix("* ").unwrap_or(text);
    let body = body
        .strip_prefix('[')
        .and_then(|r| r.split_once("] "))
        .map(|(_, rest)| rest)
        .unwrap_or(body);
    body.split(' ').next().unwrap_or(body)
}

/// Settle the invocation this result belongs to: refill its state, and for a
/// `write_file` overwrite swap the staged written-content block for a real
/// old→new diff (an overwrite is a transformation, so it earns the same
/// always-full diff treatment an edit gets).
///
/// Pairing is by tool name, first-started wins. Parallel calls materialize
/// together and finish in any order, so "the last running row" would settle
/// the *wrong* call — both its state glyph and, for `write_file`, the diff
/// swap. No call id travels on the events, so two same-name parallel calls
/// still pair FIFO.
///
/// Returns whether the result may drop the redundant tool name: only when
/// the name is uninformative — the call's block is all one tool (the name
/// would just echo the call rows) and no prose separates the call from where
/// the result lands. Mixed or far blocks keep the name as their attribution.
pub(super) fn settle_tool_call(
    output: &mut [OutputLine<ShellToken>],
    settled: ToolState,
    details: Option<&serde_json::Value>,
    tool_name: &str,
) -> bool {
    let inv_idx = output
        .iter()
        .position(|l| {
            l.kind == LineKind::Tool
                && l.tool_state == Some(ToolState::Running)
                && row_tool_name(&l.text) == tool_name
        })
        .or_else(|| {
            output
                .iter()
                .position(|l| l.kind == LineKind::Tool && l.tool_state == Some(ToolState::Running))
        });
    let Some(idx) = inv_idx else {
        return false;
    };
    // May the result drop its tool name? Only when the name carries no
    // information: the call's block (the contiguous tool-row run around it)
    // is all one tool — a name repeated there echoes the rows above — and
    // no prose separates the call from where the result lands. A mixed block
    // (parallel `execute_command` + `task_output`) keeps the name on every
    // result: it is the only attribution the interleaved rows leave.
    let mut block_start = idx;
    while block_start > 0
        && matches!(
            output[block_start - 1].kind,
            LineKind::Tool | LineKind::ToolResult
        )
    {
        block_start -= 1;
    }
    let mut block_end = idx + 1;
    while block_end < output.len()
        && matches!(
            output[block_end].kind,
            LineKind::Tool | LineKind::ToolResult
        )
    {
        block_end += 1;
    }
    let homogeneous = output[block_start..block_end]
        .iter()
        .filter(|l| l.kind == LineKind::Tool)
        .all(|l| row_tool_name(&l.text) == tool_name);
    let same_block = homogeneous
        && output[idx + 1..]
            .iter()
            .all(|l| matches!(l.kind, LineKind::Tool | LineKind::ToolResult));
    let inv = &mut output[idx];
    inv.tool_state = Some(settled);

    // Overwrite is a transformation, so it earns a diff. The tool reports
    // `write_mode` + `old_content`; the new content is what we staged on the
    // line at `ActionStarted`.
    let Some(det) = details else {
        return same_block;
    };
    if det.get("write_mode").and_then(|v| v.as_str()) != Some("overwrite") {
        return same_block;
    }
    let Some(LineDetail::Folded {
        raw: new_content, ..
    }) = inv.detail.clone()
    else {
        return same_block;
    };
    let old = det
        .get("old_content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let path = det
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    let hunks = diff_to_hunks(old, &new_content, 3);
    if !hunks.is_empty() {
        inv.detail = Some(LineDetail::Diff { path, hunks });
    }
    same_block
}
