//! Sub-agent task panel: lifecycle bookkeeping for the multi-agent fan-in.
//!
//! Split impl of `App` (methods live here, state stays on `App`): tracking
//! entries appear at spawn (registry snapshot reconciliation, Phase 5), flip
//! to done on child exit, and leave at the design's death sweeps (Rev
//! 2026-11-04 death table 死期表) — never under the user's eyes: the pinned row is
//! exempt and a sweep never takes a row the display set still owes. Also
//! owns the per-child streaming buffers (each child gets its own stream
//! state so two interleaved children can't chop each other's pending text).

use std::time::{Duration, Instant};

use phi_agent::RegistrySnapshot;
use phi_kernel_tools::background_shell::BackgroundTaskStatus;
use phi_tui::lines::{LineKind, OutputLine};
use phi_tui::stream::StreamState;
use phi_tui::transcript::DEFAULT_WRAP_WIDTH;

use crate::app::{AgentStatus, App, BackgroundTaskEntry, Phase, SubAgentState, SubAgentStatus};
use crate::task_focus::FocusTarget;
use crate::task_layout::{LeadPieces, StatusFit, fit_lead, ledger_line};
use crate::theme::ShellToken;

use super::bg_wake::{MAX_TAIL_PER_TASK, may_reap};

/// Window in which a terminal background snapshot may still be inserted for
/// rows this panel never tracked (the "finished between two polls" race): a
/// first sighting past this age is history, not news — inserting it would
/// only re-add what a sweep just dropped (and re-report a wake that already
/// fired). Entries already tracked keep receiving status updates regardless.
const TERMINAL_INSERT_WINDOW: Duration = Duration::from_secs(3);

/// Live state for the thinking panel of the focused agent: pre-wrapped tail
/// lines (unprefixed — the panel title owns agent attribution), the agent id
/// when a child, and the pending thought's char count (token estimate).
pub(crate) struct ThinkingPanelState {
    pub(crate) lines: Vec<String>,
    pub(crate) agent: Option<String>,
    pub(crate) chars: usize,
}

/// One row of the combined task panel: sub-agents first (spawn order), then
/// background shell tasks. `FocusTarget::TaskList(id)` carries [`TaskRow::id`]
/// — every consumer (render, keyboard nav, focus landing) keys rows by it,
/// never by index (Rev 2026-11-04: rows hide at terminal, indices shift).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskRow {
    SubAgent(String),
    Background(String),
}

impl TaskRow {
    /// The row's stable identity: the agent path / background task id.
    pub(crate) fn id(&self) -> &str {
        match self {
            TaskRow::SubAgent(path) => path,
            TaskRow::Background(id) => id,
        }
    }
}

impl App {
    /// Number of tracked sub-agents still `Running`.
    pub(crate) fn running_sub_agents(&self) -> usize {
        self.sub_agents
            .values()
            .filter(|s| s.status == SubAgentStatus::Running)
            .count()
    }

    /// Running background tasks split by deadline: `(bounded, daemons)`.
    /// Bounded (`timeout_ms > 0`) are jobs the agent waits on and reports;
    /// daemons (`timeout_ms == 0`) are servers that run until they die and
    /// must never hold a "waiting" state (see `BackgroundTaskEntry::is_indefinite`).
    pub(crate) fn bg_running_split(&self) -> (usize, usize) {
        let mut bounded = 0usize;
        let mut daemons = 0usize;
        for t in self.background_tasks.values() {
            if t.status != BackgroundTaskStatus::Running {
                continue;
            }
            if t.is_indefinite() {
                daemons += 1;
            } else {
                bounded += 1;
            }
        }
        (bounded, daemons)
    }

    /// The single terminal site for a sub-agent run (ENG-F1: engine events
    /// carry no outcome — the watcher wire, the lifecycle snapshot and the
    /// cancel path all land here). Idempotent: only a Running row flips, so a
    /// fact delivered twice (Progress then Batch) records once.
    ///
    /// On the flip it writes the run's **permanent** ledger marker into the
    /// root transcript (grep-able after the row is swept) and the same text
    /// as the child transcript's verdict, and — for a failure — arms the
    /// unacked red remnant.
    pub(crate) fn finalize_sub_agent(&mut self, agent_path: &str, status: SubAgentStatus) {
        if matches!(status, SubAgentStatus::Running) {
            return; // not a terminal
        }
        let Some(state) = self.sub_agents.get_mut(agent_path) else {
            return; // unknown row — nothing to finalize
        };
        if !matches!(state.status, SubAgentStatus::Running) {
            return; // already terminal (idempotent)
        }
        let elapsed = state.started_at.elapsed();
        state.status = status.clone();
        state.completed_at = Some(Instant::now());

        // The verdict follows the child's prose: flush its stream first.
        let flushed = self.flush_child_stream(agent_path);
        let verdict = ledger_line(agent_path, &status, elapsed);
        let child = self
            .sub_agent_transcripts
            .entry(agent_path.to_string())
            .or_default();
        child.extend(flushed);
        child.push(OutputLine {
            spans: None,
            original: None,
            detail: None,
            text: verdict.clone(),
            kind: LineKind::Tool,
            tool_state: None,
        });
        // Root ledger marker — any pending root text precedes it.
        self.flush_pending();
        self.transcript.push(OutputLine {
            spans: None,
            original: None,
            detail: None,
            text: verdict,
            kind: LineKind::Tool,
            tool_state: None,
        });

        if matches!(status, SubAgentStatus::Failed { .. }) {
            // The red remnant: an unacked failure stays on the face until
            // the user comes to look (frame open / row inspected). A failure
            // is the row the user most needs to see — it is why the remnant
            // replaces the old failure auto-expand (F5, retired).
            self.task_panel
                .unacked_failures
                .insert(agent_path.to_string());
        }
        self.refresh_waiting_count();
        self.sync_task_window();
        self.auto_sweep();
    }

    /// A sub-agent run reached a terminal state (watcher Progress/Batch,
    /// `RunCancelled`, lifecycle fact). Routes through
    /// `finalize_sub_agent`. Always refreshes the wait count, even for
    /// an unknown id — the refresh is itself a contract (callers can force one
    /// with a no-op id after touching the background table).
    pub fn mark_sub_agent_finished(&mut self, agent_path: &str, status: SubAgentStatus) {
        self.finalize_sub_agent(agent_path, status);
        self.refresh_waiting_count();
    }

    /// Catch-all sweep (batch injection): every still-running entry is
    /// finalized `Done` — the per-report paths have already recorded their
    /// real kinds, so what remains has no better fact.
    pub fn mark_all_sub_agents_finished(&mut self) {
        let running: Vec<String> = self
            .sub_agents
            .iter()
            .filter(|(_, s)| matches!(s.status, SubAgentStatus::Running))
            .map(|(p, _)| p.clone())
            .collect();
        for p in running {
            self.finalize_sub_agent(&p, SubAgentStatus::Done);
        }
        self.refresh_waiting_count();
    }

    /// While `Waiting`, recompute both in-flight counts from the panel
    /// (sub-agents + background tasks); drop to `Idle` only when nothing
    /// *waitable* is left (the fan-in batch injection / background wake
    /// follows). Daemons don't keep the wait alive — only bounded jobs do.
    fn refresh_waiting_count(&mut self) {
        if let AgentStatus::Waiting { .. } = self.status {
            let n = self.running_sub_agents();
            let (bounded, daemons) = self.bg_running_split();
            self.status = if n > 0 || bounded > 0 {
                // `bg` stays "all running" so the counter never under-reports;
                // the wait itself is justified by bounded jobs only.
                AgentStatus::Waiting {
                    running: n,
                    bg: bounded + daemons,
                }
            } else {
                AgentStatus::Idle
            };
        }
    }

    /// Record a sub-agent's first appearance — emitting a `* [p] started` marker
    /// and registering it in `sub_agents` — so the transcript and status strip
    /// show its lifecycle. No-op for the root agent (`agent_id == None`) and for
    /// sub-agents already seen this turn.
    pub(crate) fn track_agent(&mut self, agent_id: Option<&str>) {
        let Some(p) = agent_id.filter(|p| !p.is_empty()) else {
            return;
        };
        if !self.sub_agents.contains_key(p) {
            // Any pending root text precedes the first sub-agent event, so flush
            // it before the `started` marker to keep transcript order correct.
            self.flush_pending();
            // Extract task name from agent_id (format: "root/<task_name>")
            let name = p.split('/').next_back().unwrap_or(p).to_string();
            self.sub_agents.insert(
                p.to_string(),
                SubAgentState {
                    name,
                    status: SubAgentStatus::Running,
                    files: Vec::new(),
                    started_at: Instant::now(),
                    completed_at: None,
                    last_tool_at: Instant::now(),
                    events: Vec::new(),
                },
            );
            self.transcript.push(OutputLine {
                spans: None,
                original: None,
                detail: None,
                text: format!("* [{p}] started"),
                kind: LineKind::Tool,
                tool_state: None,
            });
        }
    }

    /// The combined panel row order: sub-agents in **spawn order** (a row
    /// must never jump under the cursor — design detail 3; tie-broken by
    /// path for same-instant spawns), then background shell tasks (registry
    /// id order). Built fresh per call — the panel is a handful of rows, and
    /// a shared list would be one more thing to keep in sync.
    fn all_task_rows(&self) -> Vec<TaskRow> {
        let mut subs: Vec<(Instant, &str)> = self
            .sub_agents
            .iter()
            .map(|(k, s)| (s.started_at, k.as_str()))
            .collect();
        subs.sort_unstable();
        subs.into_iter()
            .map(|(_, k)| TaskRow::SubAgent(k.to_string()))
            .chain(
                self.background_tasks
                    .keys()
                    .map(|k| TaskRow::Background(k.clone())),
            )
            .collect()
    }

    /// The display set (Rev 2026-11-04, row set 行集): running rows, unacked-failure
    /// remnants, dim corpses (touched rows that landed terminal) and the
    /// pinned row. A row that lands terminal untouched hides **the moment it
    /// lands** (hide-on-complete 完成即隐) — the store keeps it only until the next death
    /// sweep, so a late fan-in report still finds its entry.
    pub(crate) fn task_rows(&self) -> Vec<TaskRow> {
        self.all_task_rows()
            .into_iter()
            .filter(|r| self.row_visible(r))
            .collect()
    }

    /// Whether a row belongs to the display set (see [`Self::task_rows`]).
    fn row_visible(&self, row: &TaskRow) -> bool {
        let id = row.id();
        if self.task_panel.is_selected(id) {
            return true; // inspection freeze 检视冻结: the pinned row never leaves
        }
        match row {
            TaskRow::SubAgent(path) => {
                let Some(state) = self.sub_agents.get(path) else {
                    return false;
                };
                match state.status {
                    SubAgentStatus::Running => true,
                    _ => {
                        self.task_panel.touched.contains(id)
                            || (matches!(state.status, SubAgentStatus::Failed { .. })
                                && self.task_panel.unacked_failures.contains(id))
                    }
                }
            }
            TaskRow::Background(bg_id) => self.background_tasks.get(bg_id).is_some_and(|t| {
                t.status == BackgroundTaskStatus::Running || self.task_panel.touched.contains(id)
            }),
        }
    }

    /// Row count of the display set (frame height, window bounds).
    pub(crate) fn task_row_count(&self) -> usize {
        self.task_rows().len()
    }

    /// Whether there is anything worth a switchable row: the display set is
    /// the panel (Rev 2026-11-04 — a session with only swept history has no
    /// face; the transcript markers are its ledger).
    pub fn should_show_task_panel(&self) -> bool {
        !self.task_rows().is_empty()
    }

    /// Poll the background task registry for status updates (called every tick).
    ///
    /// Reconciles `background_tasks` against the registry's `snapshot_all()`.
    /// Also GCs completed tasks older than 3 seconds when the root is Idle.
    /// Returns `true` if any state changed (caller sets `dirty`).
    pub(crate) fn reconcile_background_tasks(&mut self) -> bool {
        // Owned clone: the sweep below (`gc`) must not hold a borrow of
        // `*self` across the `&mut self` calls in the upsert loop.
        let Some(registry) = self.background_registry.clone() else {
            return false;
        };

        // Consume-based GC with a 30-min fallback TTL (issue #35): entries
        // are swept once `task_output` has taken their output, or 30 min
        // after finish — whichever comes first. Never the old 5-min clock
        // that raced the wake's delivery.
        let gc_ttl = Duration::from_secs(1800);
        // Pure read first — `gc` runs *after* the upsert loop below, so a
        // terminal state is always observed before it can be swept away.
        let snapshots = registry.snapshot_all();
        let mut changed = false;

        // Upsert from snapshots
        let live_ids: std::collections::HashSet<String> =
            snapshots.iter().map(|s| s.id.clone()).collect();

        for snap in &snapshots {
            // Never (re-)insert a terminal snapshot this panel never tracked
            // (or already dropped) once it is past the insert window. The
            // registry keeps finished tasks until its own much longer GC TTL
            // expires, so `snapshot_all` reports long-dead work every tick —
            // inserting it would resurrect swept rows and re-arm a wake debt
            // that already fired. A fresh terminal snapshot (finished
            // between two polls) is still inserted: its wake report is owed.
            // Entries already tracked keep receiving status updates below.
            if !self.background_tasks.contains_key(&snap.id)
                && snap.status != BackgroundTaskStatus::Running
                && snap
                    .finished_at
                    .is_some_and(|at| at.elapsed() >= TERMINAL_INSERT_WINDOW)
            {
                continue;
            }
            let entry = self.background_tasks.entry(snap.id.clone());
            let is_new = !matches!(entry, std::collections::btree_map::Entry::Occupied(_));
            entry.or_insert_with(|| BackgroundTaskEntry {
                id: snap.id.clone(),
                command: snap.command.clone(),
                timeout_ms: snap.timeout_ms,
                status: BackgroundTaskStatus::Running,
                started_at: snap.started_at,
                finished_at: None,
                reported: false,
                output_tail: String::new(),
                consumed: false,
            });

            let task = self.background_tasks.get_mut(&snap.id).unwrap();
            // Consumption flips independently of status (task_output fetch).
            if task.consumed != snap.consumed {
                task.consumed = snap.consumed;
                changed = true;
            }
            if task.status != snap.status {
                task.status = snap.status.clone();
                // Use the snapshot's finished_at instead of Instant::now()
                // to preserve the real completion time across reap-reconcile cycles.
                task.finished_at = snap.finished_at;
                if snap.status != BackgroundTaskStatus::Running {
                    // First terminal observation: snapshot the output tail so
                    // the wake can report even after the registry entry is
                    // GC'd (consume-based) — one capture, not per tick.
                    task.output_tail = snap.output_tail(MAX_TAIL_PER_TASK);
                }
                changed = true;
                // A background task just started or (more importantly) ended:
                // while `Waiting` the status-bar counts must follow, and the
                // last one flipping to terminal is what arms the bg wake.
                self.refresh_waiting_count();
            } else if task.status == BackgroundTaskStatus::Running {
                // Live tail while running: the row's inspection view shows
                // output as it grows. The terminal-state capture above is the
                // final one. Only dirty the frame when the tail moved.
                let tail = snap.output_tail(MAX_TAIL_PER_TASK);
                if tail != task.output_tail {
                    task.output_tail = tail;
                    changed = true;
                }
            }
            // A fresh *running* row changes the face; a fresh terminal
            // insert is wake-debt bookkeeping only (the display set hides
            // untouched terminal rows) and must not dirty every re-add.
            if is_new && snap.status == BackgroundTaskStatus::Running {
                changed = true;
            }
        }

        // GC only after every snapshot above has been observed.
        registry.gc(gc_ttl);

        // Remove entries that were GC'd from the registry — but never one
        // that still owes the agent a wake report (see `may_reap`). The
        // registry's consume-GC can drop an entry whose outcome has not been
        // reported yet; the map keeps it until the wake delivers.
        let stale_ids: Vec<String> = self
            .background_tasks
            .iter()
            .filter(|(id, t)| !live_ids.contains(*id) && may_reap(t))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale_ids {
            self.background_tasks.remove(&id);
            changed = true;
        }

        changed
    }

    /// Reconcile the task panel against the registry's authoritative fact
    /// snapshot (`subscribe_lifecycle` watch → `TuiEvent::Lifecycle`).
    ///
    /// The registry's wire vocabulary is the sole state authority
    /// (ENG-F1 形状 B), parsed via [`phi_agent::AgentStatus::from_name`]:
    /// `queued`/`running` → create/reopen the row, `done` → Done, `failed` →
    /// `Failed { last_error }`, `cancelled`/`closed` → Cancelled, `paused`/
    /// unknown → **no fact** (the row keeps its state — never guess a
    /// terminal from a word this build does not understand). A Running row
    /// that simply vanished from the snapshot completed clean (its real
    /// outcome already rode the watcher wire). Terminal facts for rows this
    /// panel never saw are ignored (a freshly registered agent reads `done`
    /// until its first `send_task`).
    pub(crate) fn apply_lifecycle_snapshot(&mut self, snap: &RegistrySnapshot) {
        // Collect facts first — `finalize_sub_agent` takes `&mut self`.
        let mut running_rows: Vec<String> = Vec::new();
        let mut terminal_rows: Vec<(String, SubAgentStatus)> = Vec::new();
        for agent in &snap.agents {
            use phi_agent::AgentStatus as RegistryStatus;
            match RegistryStatus::from_name(&agent.status) {
                RegistryStatus::Queued | RegistryStatus::Running => {
                    running_rows.push(agent.path.clone());
                }
                RegistryStatus::Done => {
                    terminal_rows.push((agent.path.clone(), SubAgentStatus::Done));
                }
                RegistryStatus::Failed => {
                    terminal_rows.push((
                        agent.path.clone(),
                        SubAgentStatus::Failed {
                            summary: agent.last_error.clone().unwrap_or_default(),
                        },
                    ));
                }
                RegistryStatus::Cancelled | RegistryStatus::Closed => {
                    terminal_rows.push((agent.path.clone(), SubAgentStatus::Cancelled));
                }
                RegistryStatus::Paused | RegistryStatus::Unknown => {}
            }
        }
        for path in running_rows {
            // Creates the entry (with the `* started` marker) at spawn time;
            // a terminal row is reopened on re-tasking.
            self.track_agent(Some(&path));
            if let Some(state) = self.sub_agents.get_mut(&path)
                && !matches!(state.status, SubAgentStatus::Running)
            {
                state.status = SubAgentStatus::Running;
                state.completed_at = None;
            }
        }
        for (path, status) in terminal_rows {
            if self.sub_agents.contains_key(&path) {
                self.finalize_sub_agent(&path, status);
            }
        }
        // Tracked rows absent from the snapshot have unregistered: a Running
        // row that vanished without a terminal fact completed clean.
        let live: std::collections::HashSet<&str> =
            snap.agents.iter().map(|a| a.path.as_str()).collect();
        let vanished: Vec<String> = self
            .sub_agents
            .iter()
            .filter(|(path, state)| {
                !live.contains(path.as_str()) && matches!(state.status, SubAgentStatus::Running)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in vanished {
            self.finalize_sub_agent(&path, SubAgentStatus::Done);
        }
        self.refresh_waiting_count();
    }

    /// Death sweep (Rev 2026-11-04, death table 死期表): drop every terminal row the
    /// display set no longer owes — swept rows leave the store and their
    /// per-child buffers with them. Spared: running rows, the pinned row
    /// (inspection freeze 检视冻结), unacked failures (the red remnant), and background
    /// entries still owing a wake report (`may_reap`, issue #29). Untouched
    /// invisible rows go too — they are store debris, not display.
    pub fn sweep_task_rows(&mut self) -> bool {
        let selected = self.task_panel.selected_id().map(str::to_string);
        let mut removed = false;

        let subs: Vec<String> = self
            .sub_agents
            .iter()
            .filter(|(path, state)| {
                !matches!(state.status, SubAgentStatus::Running)
                    && selected.as_deref() != Some(path.as_str())
                    && !self.task_panel.unacked_failures.contains(path.as_str())
            })
            .map(|(path, _)| path.clone())
            .collect();
        for id in subs {
            self.sub_agents.remove(&id);
            self.sub_agent_transcripts.remove(&id);
            self.child_streams.remove(&id);
            self.thinking_since.remove(&id);
            self.task_panel.touched.remove(&id);
            removed = true;
        }

        let bgs: Vec<String> = self
            .background_tasks
            .iter()
            .filter(|(id, task)| {
                task.status != BackgroundTaskStatus::Running
                    && selected.as_deref() != Some(id.as_str())
                    && may_reap(task)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in bgs {
            self.background_tasks.remove(&id);
            self.task_panel.touched.remove(&id);
            removed = true;
        }

        if removed {
            self.clamp_task_focus();
            self.sync_task_window();
        }
        removed
    }

    /// All-complete sweep (Rev 2026-11-04, 全完成清场): when nothing is running, no failure is
    /// waiting for an ack, and nobody is selected, the corpses go — and with
    /// them the whole face (principle ③ 框跟着事走 — the frame follows the work). A selection blocks the sweep
    /// (a selection waits for the next death point 有选中 → 等下一死期).
    pub fn auto_sweep(&mut self) -> bool {
        let running = self.running_sub_agents()
            + self
                .background_tasks
                .values()
                .filter(|t| t.status == BackgroundTaskStatus::Running)
                .count();
        let quiet = running == 0
            && self.task_panel.unacked_failures.is_empty()
            && self.task_panel.selected_id().is_none();
        if !quiet {
            return false;
        }
        let removed = self.sweep_task_rows();
        self.teardown_task_frame_if_empty();
        removed
    }

    /// Turn-end sweep (Rev 2026-11-04, turn 收尾清场): corpses go at the root turn end —
    /// principle ② 尸体不跨 turn (corpses do not cross a turn). The pinned row is exempt (attention follows the
    /// user, not the turn — the one sanctioned principle-① exception).
    /// Focus is never touched: the set can only empty while focus is Input
    /// (a pinned row keeps itself in the set).
    pub fn turn_end_sweep(&mut self) -> bool {
        let removed = self.sweep_task_rows();
        self.teardown_task_frame_if_empty();
        removed
    }

    /// principle ③ 框跟着事走 (the frame follows the work): with no row left to show, the frame and the live
    /// face vanish together and the keyboard rests at the composer.
    fn teardown_task_frame_if_empty(&mut self) {
        if self.task_rows().is_empty() {
            self.task_panel.frame_open = false;
            self.task_panel.focus = FocusTarget::Input;
        }
    }

    /// Commit any pending streaming text/thought into the transcript (thought
    /// first, then prose). Called on structural events and turn end.
    pub(crate) fn flush_pending(&mut self) {
        let had_text = self.stream.has_pending_text();
        let had_thought = self.stream.has_pending_thought();
        // Get the pending agent BEFORE flushing (flush takes it)
        let pending_agent = self.stream.pending_agent().map(|s| s.to_string());
        let flushed = self.stream.flush();
        // Route flushed lines to the correct transcript based on pending_agent
        for line in flushed {
            if let Some(ref agent_id) = pending_agent {
                if !agent_id.is_empty() {
                    self.sub_agent_transcripts
                        .entry(agent_id.clone())
                        .or_default()
                        .push(line);
                } else {
                    self.transcript.push(line);
                }
            } else {
                self.transcript.push(line);
            }
        }
        if had_text || had_thought {
            tracing::info!(
                follow_bottom = self.viewport.follow_bottom,
                scroll_offset = self.viewport.scroll_offset,
                output_len = self.transcript.len(),
                had_text,
                had_thought,
                "flush_pending: view state"
            );
        }
        // Root flush ⇒ the root segment is over. Other streams' timers are
        // keyed separately and survive structural events that only flush the
        // root (fixes: child B's timer used to die on child A's RunFinished).
        self.thinking_since.remove("");
    }

    // ── Per-child streaming ───────────────────────────────────────────────

    /// Accumulate a child's prose delta into its own stream buffer, returning
    /// any lines the flush committed (caller routes them into the child's
    /// transcript). Lines are prefixed `[<id>] ` like the pre-split behavior.
    pub(crate) fn push_child_text(&mut self, id: &str, text: &str) -> Vec<OutputLine<ShellToken>> {
        self.child_streams
            .entry(id.to_string())
            .or_insert_with(|| StreamState::new(DEFAULT_WRAP_WIDTH))
            .push_text(text, Some(id))
    }

    /// Accumulate a child's reasoning delta into its own stream buffer.
    pub(crate) fn push_child_thought(
        &mut self,
        id: &str,
        text: &str,
    ) -> Vec<OutputLine<ShellToken>> {
        self.child_streams
            .entry(id.to_string())
            .or_insert_with(|| StreamState::new(DEFAULT_WRAP_WIDTH))
            .push_thought(text, Some(id))
    }

    /// Commit a child's pending text/thought into whole lines (e.g. before its
    /// tool-invocation line or its `done` marker, so ordering inside the
    /// child's transcript stays chronological). Empty when the child has no
    /// pending stream content.
    pub(crate) fn flush_child_stream(&mut self, id: &str) -> Vec<OutputLine<ShellToken>> {
        match self.child_streams.get_mut(id) {
            Some(st) => {
                let lines = st.flush();
                self.thinking_since.remove(id);
                lines
            }
            None => Vec::new(),
        }
    }

    /// A child's live streaming tail for the focused view: `(text, kind)` with
    /// the `[<id>] ` first-line prefix already applied, matching the shape the
    /// eventual committed line will have (`StreamState::flush_text`). Owned
    /// copy — keeps `render_output`'s borrow graph simple at ~1 clone/frame.
    pub fn child_stream_tail_raw(&self, id: &str) -> Option<(String, LineKind)> {
        let (raw, kind) = self.child_streams.get(id)?.tail_raw()?;
        Some((format!("[{id}] {raw}"), kind))
    }

    /// A child's live tail as pre-wrapped lines (the Thought-fallback path),
    /// first line prefixed like the committed lines will be.
    pub fn child_stream_tail_lines(&self, id: &str) -> Option<(Vec<String>, LineKind)> {
        let (lines, kind) = self.child_streams.get(id)?.tail_lines()?;
        let mut lines = lines.to_vec();
        if let Some(first) = lines.first_mut() {
            *first = format!("[{id}] {first}");
        }
        Some((lines, kind))
    }

    /// Live state for the thinking panel of the **focused** agent. `None` when
    /// the focused stream has no pending THOUGHT — only thought tails panel;
    /// pending prose keeps streaming inline. Root looks at `stream`, a focused
    /// child at its `child_streams` entry. Body lines are UNPREFIXED: the panel
    /// title already names the agent (`thinking - <id>`), and the `[<id>] `
    /// prefix is only for stream lines that land in the shared transcript.
    pub(crate) fn thinking_panel_state(&self) -> Option<ThinkingPanelState> {
        match &self.task_panel.focus {
            FocusTarget::Input => {
                if !self.stream.has_pending_thought() {
                    return None;
                }
                let (lines, _) = self.stream.tail_lines()?;
                let chars = self
                    .stream
                    .tail_raw()
                    .map_or(0, |(raw, _)| raw.chars().count());
                Some(ThinkingPanelState {
                    lines: lines.to_vec(),
                    agent: None,
                    chars,
                })
            }
            FocusTarget::TaskList(id) => {
                // Background rows have no thought stream — only a sub-agent
                // row can be the focused "thinking" subject.
                let TaskRow::SubAgent(agent) = self
                    .task_rows()
                    .into_iter()
                    .find(|r| r.id() == id.as_str())?
                else {
                    return None;
                };
                let stream = self.child_streams.get(&agent)?;
                let (lines, kind) = stream.tail_lines()?;
                if kind != LineKind::Thought {
                    return None;
                }
                let chars = stream.tail_raw().map_or(0, |(raw, _)| raw.chars().count());
                Some(ThinkingPanelState {
                    lines: lines.to_vec(),
                    agent: Some(agent),
                    chars,
                })
            }
        }
    }

    /// Whether the boxed (folded) thinking panel is *wanted* for the focused
    /// agent — the exact predicate `render_output` uses to choose the
    /// placeholder rows. Terminal/output-fit guards are the caller's job
    /// (they need frame geometry). Shared by the renderer so the process
    /// status line can drop the redundant "thinking..." when the panel already
    /// reports it (Ctrl+O folded view).
    pub(crate) fn thinking_panel_wanted(&self) -> bool {
        !self.show_thoughts && self.thinking_panel_state().is_some()
    }

    // ── Process-row composition (status line + squeeze ladder) ────────────

    /// Status-strip text for tool calls whose arguments are still streaming
    /// in — the calls themselves only materialize when the turn's stream
    /// drains, so this is what the user sees during that window (session
    /// 20261006_9264ba3e: four `spawn_agent` briefs, ~3.2K chars, ~25 s of
    /// visible nothing). `None` while no draft is pending.
    pub(crate) fn draft_status_text(&self) -> Option<String> {
        if self.tool_drafts.is_empty() {
            return None;
        }
        let chars: usize = self.tool_drafts.values().map(|(_, n)| n).sum();
        if self.tool_drafts.len() == 1 {
            let (name, _) = self.tool_drafts.values().next().expect("len == 1");
            let tool = if name.is_empty() {
                "tool call"
            } else {
                name.as_str()
            };
            Some(format!("writing {tool} ({chars} chars)..."))
        } else {
            Some(format!(
                "writing {} tool calls ({chars} chars)...",
                self.tool_drafts.len()
            ))
        }
    }

    /// The process row's lead (spinner/phase/waiting) split for the squeeze
    /// ladder. Wording follows the bounded/daemon split: bounded jobs carry
    /// a completion promise, daemons are merely counted — the two must never
    /// share a "waiting for completion" promise.
    pub(crate) fn status_leads(&self) -> LeadPieces {
        let activity = self.activity_suffix();
        // A pick modal is the front modal and owns input: the next step
        // is a human choice, so the status must never claim the agent is
        // thinking (Ctrl+C cancel) while the user is parked on the list.
        // Approval and ask outrank pick (one modal at a time), so only
        // report the pick when it really is the front surface.
        if self.has_pending_pick() && !self.has_pending_ask() && !self.has_pending_approval() {
            return LeadPieces {
                head: "waiting for your pick".to_string(),
                activity,
                tail: format!(" ({})", self.copy.pick_footer.trim()),
                tail_is_reassurance: true,
            };
        }
        let (head, tail) = match &self.status {
            AgentStatus::Idle => {
                let base = if let Some(n) = &self.notice {
                    n.clone()
                } else {
                    "Idle".to_string()
                };
                let (_, daemons) = self.bg_running_split();
                if daemons > 0 {
                    (base, format!(" ({daemons} daemon(s))"))
                } else {
                    (base, String::new())
                }
            }
            AgentStatus::Waiting { running, .. } => {
                let (bounded, daemons) = self.bg_running_split();
                let (head, promise) = match (*running, bounded) {
                    (_, 0) => (
                        format!(
                            "{} waiting for sub-agents ({running} running)...",
                            self.spinner_char()
                        ),
                        " auto-continue when results arrive".to_string(),
                    ),
                    (0, _) => (
                        format!(
                            "{} waiting for background tasks ({bounded} running)...",
                            self.spinner_char()
                        ),
                        " auto-report when done".to_string(),
                    ),
                    (_, _) => (
                        format!(
                            "{} waiting for sub-agents ({running}) + background tasks ({bounded})...",
                            self.spinner_char()
                        ),
                        " auto-continue when done".to_string(),
                    ),
                };
                let tail = if daemons > 0 {
                    format!("{promise} (+{daemons} daemon(s))")
                } else {
                    promise
                };
                (head, tail)
            }
            AgentStatus::Running { phase } => match phase {
                Phase::Thinking => (
                    format!("{} thinking...", self.spinner_char()),
                    " (Ctrl+C cancel)".to_string(),
                ),
                Phase::Streaming => match self.draft_status_text() {
                    // While tool-call args are being written there is no
                    // delta to stream — report the draft instead of a bare
                    // "streaming...", so the stretch reads as busy, not hung.
                    Some(draft) => (
                        format!("{} {draft}", self.spinner_char()),
                        " (Ctrl+C cancel)".to_string(),
                    ),
                    None => (
                        format!("{} streaming...", self.spinner_char()),
                        " (Ctrl+C cancel)".to_string(),
                    ),
                },
                Phase::ToolCall { tool } => match &self.live_progress {
                    Some(p) => (
                        format!("{} 🔧 {tool}: {p}", self.spinner_char()),
                        String::new(),
                    ),
                    None => (
                        format!("{} 🔧 {tool}", self.spinner_char()),
                        " (Ctrl+C cancel)".to_string(),
                    ),
                },
                Phase::AwaitingApproval => (
                    "!! waiting approval...".to_string(),
                    " (y/a/n, Ctrl+C cancel)".to_string(),
                ),
            },
        };
        LeadPieces {
            head,
            activity,
            tail,
            // Waiting/Idle tails are reassurance the ladder may shed; a
            // phase's cancel hint must survive every stage.
            tail_is_reassurance: !matches!(self.status, AgentStatus::Running { .. }),
        }
    }

    /// The process-status line (§9.6) at its wide string-table form. Tasks
    /// no longer append a segment here (Rev 2026-11-04) — they are rows on
    /// the live face / frame, not a count strip.
    pub fn status_line(&self) -> String {
        self.status_leads().full()
    }

    /// The process row squeezed for `width` columns (status-strip text +
    /// whether a Tip may still ride behind it).
    pub fn status_fit(&self, width: usize) -> StatusFit {
        fit_lead(&self.status_leads(), width)
    }
}

#[cfg(test)]
#[path = "task_panel_bg_tests.rs"]
mod task_panel_bg_tests;
#[cfg(test)]
#[path = "task_panel_snapshot_tests.rs"]
mod task_panel_snapshot_tests;
#[cfg(test)]
#[path = "task_panel_tests.rs"]
mod task_panel_tests;
