//! TUI application state: the independent `RuntimeEvent` consumer.
//!
//! The agent loop (phi-agent) is untouched; its `RuntimeEvent`s are forwarded
//! here through a channel and mapped onto visible state — a scrollable output
//! buffer (tool calls render inline), a status state machine, and the composer.
//! This is the same event stream the `print_event` REPL renderer consumed, so
//! nothing in the framework needed to change for the TUI to exist.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use phi_agent::{ApprovalDecision, ApprovalRequest, RegistrySnapshot, RuntimeEvent};
use phi_kernel_tools::background_shell::{BackgroundTaskRegistry, BackgroundTaskStatus};
use ratatui::style::{Modifier, Style};

use crate::approval::{ApprovalItem, ApprovalModeSwitch, RuntimeApprovalMode};
use crate::handlers::mouse::DragState;
use crate::interaction::{AskAnswer, AskItem, PickAnswer, PickItem};
use crate::shell::{ConsentProbe, CopyTable, Keymap};
use crate::theme::{BrandRow, ShellToken, SpanSpec, Theme};
use crate::tips::TipItem;
use phi_tui::completer::{CompleterAction, MentionCompleter, SlashCompleter};
use phi_tui::input::Composer;
use phi_tui::lines::{LineKind, OutputLine};
use phi_tui::popup_list::PopupStyle;
use phi_tui::selection::SelectionState;
use phi_tui::stream::StreamState;
use phi_tui::transcript::{DEFAULT_WRAP_WIDTH, Transcript};
use phi_tui::viewport::Viewport;
use phi_tui::visual::VisualMap;
use phi_tui::wrap::wrap;

mod compose;
mod event;
mod interaction;

// Selection / ContextMenu / context_menu_pos / CONTEXT_MENU_* moved to
// selection.rs; re-exported here for the renderer and tests.
#[allow(unused_imports)] // re-exported for external consumers (tests use it via super::*)
pub use phi_tui::selection::{
    CONTEXT_MENU_H, CONTEXT_MENU_W, CharPoint, ContextMenu, Selection, context_menu_pos,
};

/// The phase of a running turn, driven by the event stream (§9.6 state machine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Thinking,
    Streaming,
    ToolCall { tool: String },
    AwaitingApproval,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    /// The root turn ended but work is still in flight — sub-agents
    /// (`running`, fan-in wake) and/or background shell tasks (`bg`, bg
    /// wake). The two counts are independent: either alone, or both, keep
    /// the agent in `Waiting` instead of dropping to `Idle`.
    Waiting {
        running: usize,
        bg: usize,
    },
    Running {
        phase: Phase,
    },
}

/// Summary of a finished turn. Rendered as the turn-final status line at the
/// end of the transcript ("crunched for 1m 18s - 15.0k tok - done 20:11", grey; a failed
/// turn reads "- error" in dark red) so scrolling back through history shows
/// how every turn ended. Cleared when the next turn begins.
#[derive(Debug, Clone)]
pub(crate) struct LastTurnSummary {
    /// Total elapsed time of the finished Running/Waiting stretch.
    pub duration: Duration,
    /// Wall-clock time when the stretch settled to Idle.
    pub finished_at: chrono::NaiveTime,
    /// True when the turn was cancelled (Ctrl+C) rather than completed.
    pub cancelled: bool,
    /// Tokens consumed by the finished stretch (real LLM usage accumulated
    /// from `TuiEvent::Usage`); `0` when no usage arrived.
    pub tokens: u64,
}

/// How a turn ended — picks the wording and color of the turn-final status
/// line appended at settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnEnd {
    /// The model finished normally (`- done`).
    Done,
    /// The run failed or was cut short by the turn cap (`- error`, dark red).
    Error,
    /// A local model command answered without running the model. Settles the
    /// turn but records no summary and appends no final-status line — there
    /// was no run to summarize.
    Command,
}

/// Live state of a sub-agent shown in the task panel — the display
/// vocabulary lives in [`crate::task_layout`] (glyph table, ledger markers
/// and counts all speak it); re-exported here for render/tests.
pub use crate::task_layout::SubAgentStatus;

/// A background shell task tracked in the task panel.
#[derive(Debug, Clone)]
pub struct BackgroundTaskEntry {
    pub id: String,
    pub command: String,
    /// Per-call timeout fuse in ms — mirrors the registry's snapshot. The
    /// sole classifier for `Self::is_indefinite`: `0` means the task was
    /// launched daemon-style (no fuse), not that a fuse is pending.
    pub timeout_ms: u64,
    pub status: BackgroundTaskStatus,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
    /// Whether this task's completion has already been included in a
    /// background-wake injection (`take_bg_wake`). One task is reported to
    /// the agent at most once — a later wake (new tasks spawned after the
    /// report turn) lists only the not-yet-reported ones.
    pub reported: bool,
    /// Last ≤2KB of stdout+stderr: live while the task runs (refreshed each
    /// reconcile tick — the row's inspection view tails it), then captured
    /// final at the terminal state. The wake is self-sufficient with it: even
    /// if the registry entry is later GC'd (consume-based), the agent can
    /// still report from this tail.
    pub output_tail: String,
    /// Whether `task_output` already returned this task's terminal output.
    /// Consumed tasks' registry entries are GC-eligible immediately; the wake
    /// still reports them (from `output_tail`), it just won't ask for a fetch.
    pub consumed: bool,
}

impl BackgroundTaskEntry {
    /// Daemon-style task (`timeout_ms == 0`): a server/watcher that runs until
    /// it dies. Not "work in flight" — there is nothing to wait on and no
    /// completion to promise. Its exit is still the interesting event (a died
    /// server must be reported), so it stays wake-worthy; it just never holds
    /// the user in a fake "waiting" state.
    pub(crate) fn is_indefinite(&self) -> bool {
        self.timeout_ms == 0
    }
}

/// A tool call event from a sub-agent (for detail display).
#[derive(Debug, Clone)]
pub struct ToolEvent {
    pub tool_name: String,
    pub summary: String,
    pub is_finished: bool,
}

/// Detailed state of a sub-agent for the task panel.
#[derive(Debug, Clone)]
pub struct SubAgentState {
    /// Task name (from spawn_agent name argument)
    pub name: String,
    /// Current status
    pub status: SubAgentStatus,
    /// Key files (from spawn_agent task argument)
    pub files: Vec<String>,
    /// When the sub-agent started
    pub started_at: Instant,
    /// When the sub-agent completed (for auto-removal timing)
    pub completed_at: Option<Instant>,
    /// Last time a tool event (started or finished) arrived. Drives the
    /// panel's `writing...` hint — a Running agent whose tool feed has been
    /// quiet this long is generating prose/thought between tool calls.
    pub last_tool_at: Instant,
    /// Tool call events (for detail display)
    pub events: Vec<ToolEvent>,
}

/// Running-agent quiet period before the activity column switches from the
/// (stale) last tool event to `writing...`. Chosen above typical inter-tool
/// gaps so it never flashes during an active tool loop.
pub(crate) const CHILD_WRITING_HINT_AFTER: Duration = Duration::from_secs(20);

/// Lines per wheel tick. 1 is the finest step the terminal's line grid
/// allows — a trackpad swipe is a stream of ticks, so it composes into the
/// smoothest possible motion; PgUp/PgDn stay the fast path for history.
pub(crate) const WHEEL_STEP: usize = 1;

/// Whether the panel should show `writing...` for this sub-agent: it is
/// Running, its last tool call has finished (or none yet), and the tool feed
/// has been quiet for [`CHILD_WRITING_HINT_AFTER`]. An in-flight tool keeps
/// its own honest `→ tool` display.
pub(crate) fn is_writing_hint(state: &SubAgentState, now: Instant) -> bool {
    if !matches!(state.status, SubAgentStatus::Running) {
        return false;
    }
    if state.events.last().is_some_and(|e| !e.is_finished) {
        return false;
    }
    now.duration_since(state.last_tool_at) >= CHILD_WRITING_HINT_AFTER
}

/// Task panel UI state (`TaskPanel`) and its focus vocabulary — defined in
/// [`crate::task_focus`] (the three-state toggle machine owns them);
/// re-exported here for render/tests.
pub use crate::task_focus::{FocusTarget, TaskPanel};

/// How much of a folded tool block is on screen (Ctrl+E cycles these).
///
/// The three tiers are a *preview ladder*, not three different payloads: every
/// tier draws from the same `LineDetail::Folded.raw`, so scrolling stays cheap
/// while the full text is always one keypress away. Diff blocks are deliberately
/// outside this ladder — an edit is a transformation and its evidence is never
/// previewed away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResultTier {
    /// Meta line + `... +N lines`. The scrolling default: you see what ran and
    /// how much came back, without the payload competing for the pane.
    #[default]
    Compact,
    /// Meta line + 2 preview rows + the overflow hint.
    Default,
    /// The whole block. What you switch to when reading one result closely.
    Expanded,
}

impl ResultTier {
    /// Next tier in the Ctrl+E cycle: compact → default → expanded → compact.
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Compact => Self::Default,
            Self::Default => Self::Expanded,
            Self::Expanded => Self::Compact,
        }
    }
}

/// A user action surfaced from key handling, consumed by the TUI loop.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Submit(String),
    Quit,
    Cancel,
    Approve(ApprovalDecision),
    /// Copy the assistant's last reply to the system clipboard (Ctrl+Y).
    CopyLastReply,
    /// Copy the active transcript selection to the system clipboard (Ctrl+C
    /// with a selection, or right-click → copy).
    CopySelection,
    /// Grab the clipboard's image and queue it as a pending attachment chip
    /// (Ctrl+V). No image on the clipboard falls back to a plain text paste.
    PasteImage,
    /// Force a full screen repaint (Ctrl+L). Heals terminal-grid desync the
    /// diff renderer cannot see — clipped cells after a size pulse, stale
    /// composites after an alt-screen bounce — by invalidating ratatui's cache
    /// (see the `Event::Resize` note in `run.rs`).
    Redraw,
    /// Reopen / rebind the design canvas in the browser (Ctrl+B, 1C row 16).
    /// The user's own consent — launches without the `y` gate; after a failure
    /// it rebinds a fresh server first.
    CanvasReopen,
    /// Queue the composer text for the running turn (Enter while running),
    /// never a silent drop. Plain text goes into the engine's steering queue
    /// and reaches the model at the next tool boundary; a `/` command cannot
    /// steer (a raw `/foo` user message is just text to the model) so it waits
    /// in [`App::queued_cmds`] and runs as its own turn after this one settles.
    Steer(String),
    /// Take the not-yet-delivered steering messages back into the composer
    /// (Up from the first line while messages are queued).
    TakeBackQueued,
    /// Queue the composer text like [`Action::Steer`], then interrupt the
    /// running turn so the queued messages go out right away (Ctrl+Enter).
    /// With nothing running this is just a submit.
    ForceSend(String),
}

/// What a completion picker answered, normalized across the two completer
/// action types (`MentionCompleter`'s `Entry` vs `SlashCompleter`'s skill
/// tuple) so the shared key skeleton can branch on one enum.
enum CompletionStep {
    Cancelled,
    /// Selection finished; carries the replacement text the twin's policy
    /// line chose (a finished path for mention, `/{name} ` for slash).
    Selected(String),
    Continue,
    Unhandled,
}

/// Policy knobs where the mention and slash pickers deliberately differ.
/// The whole twin delta lives here and in the two one-line selection formats —
/// everything else is the one shared [`App::completion_key`] skeleton
/// (§7.2: one copy of the mechanism, one-line policy deltas).
struct CompletionPolicy {
    /// Esc with a typed prefix keeps the typed text instead of wiping
    /// `trigger + prefix`. Mention: the path is already visible in the
    /// composer and is often wanted. Slash: cancel discards the draft.
    esc_keeps_text: bool,
}

impl CompletionPolicy {
    const MENTION: Self = Self {
        esc_keeps_text: true,
    };
    const SLASH: Self = Self {
        esc_keeps_text: false,
    };
}

/// An event delivered from the agent task to the TUI.
#[derive(Debug)]
pub enum TuiEvent {
    Runtime(RuntimeEvent),
    /// Registry lifecycle snapshot (Phase 5): the authoritative fact view of
    /// every registered sub-agent, forwarded from `subscribe_lifecycle`. The
    /// task panel reconciles against this instead of inferring state from
    /// child runtime events.
    Lifecycle(std::sync::Arc<RegistrySnapshot>),
    /// A turn ended with an error (surfaced as a red output line).
    ///
    /// Terminal: settles the turn and clears [`App::running`].
    TurnError(String),
    /// A mid-turn warning (surfaced as a red output line) that does NOT end
    /// the turn — e.g. a skill-bake failure while the run continues. Unlike
    /// [`TuiEvent::TurnError`] it never settles, so it cannot trick the UI
    /// into believing the turn finished early.
    Warning(String),
    /// A turn completed (safety net in case `RunFinished` was never seen).
    TurnDone,
    /// A turn ended by user cancel (Ctrl+C). Settles like [`TuiEvent::TurnDone`]
    /// but stamps the `cancelled` verdict on the turn-final status line
    /// directly, so the summary is right even when the engine's
    /// `RunCancelled` (a separate bus hop) loses the arrival race.
    TurnCancelled,
    /// A local model command's answer (e.g. `/model`), delivered as a system
    /// line. Settles the turn like [`TuiEvent::TurnDone`] (clears
    /// [`App::running`]) but adds no turn-final status line — the command
    /// never ran the model.
    SystemMessage(String),
    /// A system line that settles nothing — the mid-turn command lane's
    /// answer, delivered while a model turn is still streaming behind it
    /// ([`TuiEvent::SystemMessage`] would falsely settle that turn).
    SystemLine(String),
    /// A product-composed status hint (e.g. an upgrade notice) — fully
    /// worded product-side. Committed to the transcript and parked in the
    /// status bar's right slot.
    StatusHint(String),
    /// A tips refresh delivered a new content version (the background fetch
    /// task sends this; same-version fetches only bump the cache timestamp
    /// and never reach the UI).
    TipsUpdated {
        items: Vec<TipItem>,
    },
    /// Real LLM token usage for one model call, forwarded from the agent
    /// task's `on_turn_end` hook. Accumulated into the status bar's token
    /// readout and the turn-final status line.
    Usage {
        /// Prompt/input tokens of the call.
        input_tokens: u64,
        /// Completion/output tokens of the call (thinking included).
        output_tokens: u64,
    },
    /// A product hook degraded to its fallback (T12 triage), reported from a
    /// task that cannot reach the frame flipbook (the agent loop). The run
    /// loop consumes this and writes the trace mark next to the frames it
    /// affected; the payload is the hook's name.
    Degrade(&'static str),
}

// No `Debug` derive: `App` holds the product's fn-slot handles (e.g.
// `ConsentProbe`), which are `Arc<dyn Fn>` and deliberately opaque. Nothing
// formats an `App`; test snapshots print selected fields instead.
/// One pasted screenshot waiting in the composer: its capture file under the
/// session's `captures/` dir plus the header-read dimensions for the chip.
/// Sent with the next submit as a user-message image attachment.
#[derive(Clone, Debug)]
pub struct PendingImage {
    /// The capture file the attachment reads at send time.
    pub path: PathBuf,
    /// Header dimensions (no pixel decode) — chip label only.
    pub width: u32,
    /// Header dimensions (no pixel decode) — chip label only.
    pub height: u32,
}

/// What a Ctrl+V did — the caller's only follow-up is [`Self::NoImage`]
/// (fall back to a text paste); the other outcomes are already answered
/// with a chip or an error line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteOutcome {
    /// A screenshot landed on the chip row.
    Attached,
    /// The clipboard holds no image — try text paste.
    NoImage,
    /// Nothing was pasted; the reason is on screen (notice / error line).
    Rejected,
}

pub struct App {
    /// Committed transcript: output lines, the plan block, and wrap width.
    pub transcript: Transcript<ShellToken>,
    pub composer: Composer,
    pub status: AgentStatus,
    /// True while a root turn is in flight or queued (gates submitting
    /// another task). Set at every `Cmd::Run` send site — synthetic turns
    /// (background wake / child fan-in) included — and cleared only by
    /// `settle_after_turn` (i.e. exactly one TurnDone/TurnError per
    /// turn), so a late engine event can never clear the next turn's flag.
    /// While set, Enter queues a message for the turn instead of submitting
    /// (see [`Action::Steer`]).
    pub running: bool,
    /// Steering messages queued while a turn runs (Enter), FIFO. The engine's
    /// steering queue is the source of truth — this mirror is refreshed from
    /// `AgentRuntime::steering_snapshot` by `sync_steer_queue` every tick and
    /// lists exactly what has not yet been delivered at a tool boundary.
    pub queued_steer: Vec<String>,
    /// Slash commands queued while a turn runs. They cannot ride the steering
    /// queue (the model would read `/review` as plain text), so they wait here
    /// and run as their own turns in order after the current one settles.
    pub queued_cmds: Vec<String>,
    /// Commands executing on the mid-turn lane, keyed by their raw input: the
    /// claim set a spawned action releases when it answers. A repeat of a
    /// claimed command is one impatient key, not a second intent.
    pub(crate) mid_commands: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Input history (#7): the fish-style recall ring — the draft you were
    /// typing is a slot in the ring, so ↑ past the oldest finds it back.
    /// Pushed at every send, recalled with ↑/↓ (Alt+↑/↓ keep cursor moves).
    pub(crate) input_history: phi_tui::history::HistoryRing,
    /// Persistence behind the ring (`ShellSpec::input_history`): `None` =
    /// the ring lives for this session only.
    input_history_store: Option<crate::input_history::InputHistoryStore>,
    /// Scroll-companion arrow suppression (`ShellSpec::scroll_arrow_guard`):
    /// drops the bare ↑/↓ a terminal synthesizes alongside wheel events so
    /// scrolling never flips input history. Off = every key passes.
    pub(crate) scroll_guard: crate::scroll_guard::ScrollArrowGuard,
    /// Cancel-rescue arm (#6), `(echo_start, echo_end, text, images)`: the
    /// root submit's transcript echo span, the text itself, and the pasted
    /// screenshots that rode it. Armed by
    /// [`App::begin_rescuable_turn_with_images`], consumed by
    /// [`App::take_cancel_rescue`] — Ctrl+C before any visible output puts
    /// the message back in the composer (chips included) and un-echoes it,
    /// as if never sent. Synthetic turns arm nothing: their echo is not a
    /// typing mistake to fix.
    cancel_rescue: Option<(usize, usize, String, Vec<PendingImage>)>,
    /// Set when a cancel-rescue just fired, so the turn's late `RunCancelled`
    /// settles quiet (no "cancelled" row, no "- cancelled" summary) instead of
    /// leaving a marker for a turn the transcript no longer admits happened.
    pub(crate) rescued_last_turn: bool,
    /// Scrollable output viewport (offset + follow-bottom + rendered size).
    pub viewport: Viewport,
    /// Pending approval requests (front = the popup currently shown).
    pub approval_queue: VecDeque<ApprovalItem>,
    /// Pending `ask_user` questions (front = the popup currently shown).
    /// Same one-at-a-time queue shape as approvals: the tool is parked on its
    /// oneshot until the user picks or skips, and the transcript stays visible
    /// behind the window — the question is a popup, never a screen takeover.
    pub ask_queue: VecDeque<AskItem>,
    /// Live approval mode switch, shared with the approval gate (Shift+Tab
    /// flips it; the status bar badges the current mode).
    pub approval_switch: Arc<ApprovalModeSwitch>,
    /// Product consent-gate classifier (ShellSpec `is_consent`): which approval
    /// requests wear the consent (two-key) face instead of the tool one.
    pub(crate) is_consent: Option<ConsentProbe>,
    /// Vision capability probe (ShellSpec `vision`): `Some(false)` stops the
    /// paste pipeline before it writes a capture (the model cannot see the
    /// picture); `None`/`Some(true)` let it through.
    pub(crate) vision: Option<crate::shell::VisionProbe>,
    /// Screenshots pasted (Ctrl+V) and not yet sent ([`PendingImage`]).
    /// Rendered as chips above the composer; consumed by the submit site.
    pub pending_images: Vec<PendingImage>,
    /// Live sub-agent states, keyed by `agent_id` (`root/<task_name>`).
    pub sub_agents: BTreeMap<String, SubAgentState>,
    /// Task panel UI state (focus, selection).
    pub task_panel: TaskPanel,
    /// Sub-agent transcripts (separate from main transcript).
    /// Key: agent_id, value: output lines for that sub-agent.
    pub sub_agent_transcripts: BTreeMap<String, Vec<OutputLine<ShellToken>>>,
    /// Mouse selection + right-click copy menu.
    pub selection_state: SelectionState,
    /// Left-drag tracking: button down, pane-edge arming (auto-scroll), the
    /// press point a multi-click pick must re-anchor at on the first drag.
    pub(crate) drag: DragState,
    /// Timestamp of the last "press Ctrl+C again to exit" hint. If set and
    /// within `QUIT_HINT_TIMEOUT`, a second Ctrl+C actually quits.
    pub(crate) quit_hint_at: Option<Instant>,
    /// Selected option for the approval popup (BUG 4, final form).
    ///
    /// A typed letter must never decide a prompt. The first gate (500ms
    /// isolation gap) still fired on the *first* key of a typing burst —
    /// `anyday`'s `a` granted AllowAlways (acceptance session
    /// 20261018_0affd119) — because a lone decision key and the first letter
    /// of a word have the same timing signature. So the decision moved to a
    /// commit step typing never produces: bare Enter. y/a/n only highlight a
    /// choice here; Enter commits it. Esc does nothing (an IME cancel sends
    /// Esc). Cleared whenever the front prompt changes.
    pub(crate) approval_selection: Option<ApprovalDecision>,
    /// Cursor row of the ask popup (`ask_user`). Unlike the approval letter
    /// highlight this is a list cursor the user moves with ↑↓ — Enter commits
    /// whatever is under it. Reset whenever the front prompt changes.
    pub(crate) ask_selection: usize,
    /// The ask popup's clarify input buffer ("Chat about this" pseudo-row):
    /// `Some` while the popup shows the one-line question box instead of the
    /// option list. Its own buffer — the composer draft is never touched.
    /// Cleared whenever the front prompt changes.
    pub(crate) ask_clarify: Option<String>,
    /// Pending product picks (front = the modal currently shown). The queue
    /// shape mirrors asks: the awaiting command parks on its oneshot until
    /// the user confirms a row or dismisses.
    pub pick_queue: VecDeque<PickItem>,
    /// The pick modal's typed filter: prefix, the row indices that survive
    /// it, and the cursor among them. `None` while no modal is up; rebuilt
    /// whenever the front item changes.
    pub(crate) pick_filter: Option<phi_tui::picker::Picker<usize>>,
    /// `@` mention picker, open while the user is choosing a path.
    pub(crate) mention: Option<MentionCompleter>,
    /// `/` skill picker, open while the user is choosing a skill.
    pub(crate) slash: Option<SlashCompleter>,
    /// Band style for the `@`/`/` pickers (wired from `ui.popup` config).
    pub(crate) popup_style: PopupStyle,
    /// Loaded skill summaries (name, description) from `SkillResolver`, powering the `/` picker.
    pub(crate) skill_summaries: Vec<(String, String)>,
    /// The workspace root — in-workspace paths render relative to it.
    pub workspace_root: PathBuf,
    /// The semantic color stylesheet (startup wiring from `ShellSpec.theme`).
    /// Class-name run tokens resolve their colors against it at render time;
    /// literal (brand) runs ignore it.
    theme: Theme,
    /// Product-semantic key bindings (`ShellSpec.keymap`, design §5.2). The
    /// fixed interaction protocol keys are read from constants, not here.
    pub(crate) keymap: Keymap,
    /// Product wording for the shell chrome (`ShellSpec.copy`).
    pub(crate) copy: CopyTable,
    /// Output pane rect `(x, y, w, h)` in cells, refreshed each draw so mouse
    /// events can be hit-tested against the transcript.
    pub(crate) output_area: Option<(u16, u16, u16, u16)>,
    /// Visual-row map for the current frame (visual row -> output index +
    /// plain-text twin), rebuilt by `render_output` each frame. Feeds mouse
    /// hit-testing (`line_index_at`), copy (`selection_text`) and the
    /// block-aware scroll anchor (`phi_tui::visual`).
    pub(crate) visual_map: VisualMap,
    /// Live streaming tail state (pending text/thought + incremental wrap cache).
    /// Root-agent deltas only — see `child_streams` for why children are separate.
    pub(crate) stream: StreamState<ShellToken>,
    /// Per-child streaming accumulators. Child deltas never touch the shared
    /// `stream`: that buffer's pending tail renders as the main view's live
    /// tail, so a child streaming for minutes flooded it at ~10 events/sec
    /// (session 20260904_3eeb5610, "kept redrawing"). Per-child buffers also stop two
    /// interleaved children from chopping each other's pending text into
    /// fragments on every agent switch. Flushed lines land in
    /// `sub_agent_transcripts` (the child's focus view).
    pub(crate) child_streams: BTreeMap<String, StreamState<ShellToken>>,
    /// Latest live tool-progress line (e.g. streaming `execute_command` output),
    /// shown in the status bar and cleared when the tool call finishes.
    pub(crate) live_progress: Option<String>,
    /// Tool calls the model is still writing arguments for, keyed by the
    /// call's stream index → `(function name, args chars so far)`. Root only.
    ///
    /// A turn's tool calls only materialize once the whole stream drains, so
    /// during a long argument dump (a `spawn_agent` task brief can run past
    /// 3K chars) this map is the only liveness signal — see
    /// `RuntimeEvent::ActionDraft` and `draft_status_text`. Cleared when the
    /// calls materialize (root `ActionStarted`) or the turn settles.
    pub(crate) tool_drafts: HashMap<usize, (String, usize)>,
    /// Transient status-bar notice (e.g. "📋 copied …"), cleared on the next key.
    pub(crate) notice: Option<String>,
    /// Persistent upgrade hint (e.g. "⬆ phimint 0.2.0 available - run: brew
    /// upgrade phimint"), rendered on the bottom status bar's right slot.
    /// Survives keypresses; cleared on /upgrade or when already up-to-date.
    pub(crate) upgrade_hint: Option<String>,
    /// Product panel face for the bottom status bar's left slot (1C row 13),
    /// refreshed each frame through the tick seam. `None` until a panel has
    /// something to report.
    pub(crate) panel_status: Option<crate::shell::StatusFace>,
    /// When the current Running/Waiting stretch began — drives the spinner and
    /// the elapsed readout in the status bar. `None` while Idle. The event loop
    /// has no other clock source: with zero events it never redraws, so a
    /// silent LLM call used to freeze the whole screen (session
    /// 20260904_e6612477, "frozen for a while"). The ~250 ms tick re-renders while this is
    /// set, keeping spinner/elapsed alive through event-less stretches.
    pub(crate) activity_since: Option<Instant>,
    /// Summary of the last finished turn (rendered as the transcript's
    /// turn-final status line); cleared as soon as the next turn's activity
    /// begins.
    pub(crate) last_turn_summary: Option<LastTurnSummary>,
    /// True while `last_turn_summary` holds a settled stretch whose
    /// turn-final status line has not been appended yet. Set when
    /// `settle_status_from_inflight` records the summary (which can happen
    /// before the settling `TurnDone`/`TurnError` arrives — the `RunCancelled`
    /// root handler settles early), consumed exactly once by
    /// `settle_after_turn`.
    pub(crate) final_status_pending: bool,
    /// Transient flag set by the `RunCancelled` root handler so the next
    /// `settle_status_from_inflight` → Idle transition records a cancelled
    /// summary instead of a normal "done" one.
    pub(crate) pending_cancel: bool,
    /// Fold state for committed thought blocks: `false` = collapsed summary
    /// lines, `true` = full text inline. Global toggle (Ctrl+O); folding is a
    /// render-time decision — `OutputLine` always carries the full text.
    /// Consumed by the thought-fold renderer (render.rs).
    pub(crate) show_thoughts: bool,
    /// How much of a folded tool block (result text, written file content) is
    /// shown. Cycled by Ctrl+E. Render-time only — the line always carries the
    /// full text in `LineDetail::Folded.raw`, so the transcript, selection and
    /// frame capture are unaffected by the tier.
    pub(crate) result_tier: ResultTier,
    /// When each stream's current pending thought segment began, keyed by
    /// agent path (`""` = the root stream, matching the `agent_id` routing
    /// convention). A segment's first delta inserts, its owning flush
    /// removes; drives the thinking panel's elapsed readout.
    pub(crate) thinking_since: HashMap<String, Instant>,
    /// Background task registry (injected from agent::build).
    pub(crate) background_registry: Option<Arc<BackgroundTaskRegistry>>,
    /// Background shell tasks displayed in the task panel.
    pub(crate) background_tasks: BTreeMap<String, BackgroundTaskEntry>,
    /// Rotating tips pool: built-in defaults, replaced when a cloud fetch
    /// delivers a new content version. Empty when `ui.tips.enabled` is false.
    pub(crate) tips: Vec<TipItem>,
    /// Round-robin cursor into `tips`, advanced once per settled turn so each
    /// Idle stretch greets the next tip. Always `< tips.len()` when non-empty.
    pub(crate) tip_index: usize,
    /// Session-cumulative real LLM token usage (prompt/completion), fed by
    /// `TuiEvent::Usage`. Rendered in the status bar's activity suffix.
    pub(crate) tokens_in: u64,
    pub(crate) tokens_out: u64,
    /// Tokens consumed by the current activity stretch; reset when a new
    /// stretch begins and snapshotted into `LastTurnSummary`.
    pub(crate) turn_tokens: u64,
}

/// `950` → "950"; `12000` → "12.0k". Cosmetic counts in titles/summaries.
pub(crate) fn fmt_k(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

/// Write pasted bytes into the session's `captures/` dir as a typed capture
/// file. The magic bytes decide PNG vs JPEG; anything else is refused before
/// any write.
pub(crate) fn save_capture(
    bytes: &[u8],
    session_dir: &std::path::Path,
) -> Result<PendingImage, String> {
    let mime = phi_agent::image_ops::sniff_mime(bytes)
        .ok_or_else(|| "clipboard content is not a PNG/JPEG image".to_string())?;
    let (width, height) = phi_agent::image_ops::header_dimensions(bytes)
        .ok_or_else(|| "unreadable image header".to_string())?;
    let ext = if mime == "image/png" { "png" } else { "jpg" };
    let dir = session_dir.join("captures");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("paste-{stamp}-{width}x{height}.{ext}"));
    std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(PendingImage {
        path,
        width,
        height,
    })
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            transcript: Transcript::new(),
            composer: Composer::new(),
            status: AgentStatus::Idle,
            running: false,
            queued_steer: Vec::new(),
            queued_cmds: Vec::new(),
            mid_commands: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            cancel_rescue: None,
            rescued_last_turn: false,
            input_history: phi_tui::history::HistoryRing::new(),
            input_history_store: None,
            scroll_guard: crate::scroll_guard::ScrollArrowGuard::default(),
            viewport: Viewport::new(),
            approval_queue: VecDeque::new(),
            ask_queue: VecDeque::new(),
            approval_switch: Arc::new(ApprovalModeSwitch::new(RuntimeApprovalMode::Auto)),
            is_consent: None,
            vision: None,
            pending_images: Vec::new(),
            sub_agents: BTreeMap::new(),
            task_panel: TaskPanel::default(),
            sub_agent_transcripts: BTreeMap::new(),
            selection_state: SelectionState::new(),
            drag: DragState::default(),
            quit_hint_at: None,
            approval_selection: None,
            ask_selection: 0,
            ask_clarify: None,
            pick_queue: VecDeque::new(),
            pick_filter: None,
            mention: None,
            slash: None,
            popup_style: PopupStyle::default(),
            skill_summaries: Vec::new(),
            workspace_root: PathBuf::new(),
            theme: Theme::default(),
            keymap: Keymap::default(),
            copy: CopyTable::default(),
            output_area: None,
            visual_map: VisualMap::new(),
            stream: StreamState::new(DEFAULT_WRAP_WIDTH),
            child_streams: BTreeMap::new(),
            live_progress: None,
            tool_drafts: HashMap::new(),
            notice: None,
            upgrade_hint: None,
            panel_status: None,
            activity_since: None,
            last_turn_summary: None,
            final_status_pending: false,
            pending_cancel: false,
            show_thoughts: false,
            result_tier: ResultTier::default(),
            thinking_since: HashMap::new(),
            background_registry: None,
            background_tasks: BTreeMap::new(),
            tips: Vec::new(),
            tip_index: 0,
            tokens_in: 0,
            tokens_out: 0,
            turn_tokens: 0,
        }
    }

    /// Inject the background task registry (from agent::build).
    pub fn set_background_registry(&mut self, registry: Arc<BackgroundTaskRegistry>) {
        self.background_registry = Some(registry);
    }

    /// Mount the product's vision probe (ShellSpec `vision`).
    pub fn set_vision(&mut self, probe: crate::shell::VisionProbe) {
        self.vision = Some(probe);
    }

    /// Arm the scroll-companion arrow guard (ShellSpec `scroll_arrow_guard`).
    /// Products opt in; the default is every key passes unchanged.
    pub fn set_scroll_arrow_guard(&mut self, enabled: bool) {
        self.scroll_guard = crate::scroll_guard::ScrollArrowGuard::new(enabled);
    }

    /// Feed a drained input batch to the guard BEFORE dispatching it: the
    /// wheel pre-scan is what makes companion-arrow suppression
    /// order-independent (the terminal may lead with either stream). Called
    /// once per input drain by the run loop.
    pub fn note_input_batch(&mut self, batch: &[crossterm::event::Event]) {
        self.scroll_guard.note_batch(batch, Instant::now());
    }

    /// What the product's vision probe says: `Some(false)` = the current model
    /// cannot see pictures, `Some(true)` = it can, `None` = unknown.
    pub fn vision_sees_images(&self) -> Option<bool> {
        self.vision.as_ref().and_then(|probe| probe())
    }

    /// Chip labels for the renderer: one per pending image.
    pub fn pending_image_labels(&self) -> Vec<String> {
        self.pending_images
            .iter()
            .map(|img| format!("📷 {}x{}", img.width, img.height))
            .collect()
    }

    /// Consume every pending image (submit site) — the chips clear with the
    /// send.
    pub fn take_pending_images(&mut self) -> Vec<PendingImage> {
        std::mem::take(&mut self.pending_images)
    }

    /// Drop the last pending image (Backspace on an empty composer).
    pub fn pop_pending_image(&mut self) -> Option<PendingImage> {
        self.pending_images.pop()
    }

    /// Store pasted clipboard bytes as a session capture and queue the chip.
    /// Captures land under `session_dir/captures/` (created on first paste);
    /// the type comes from the magic bytes, never the name.
    pub fn paste_image_bytes(
        &mut self,
        bytes: &[u8],
        session_dir: &std::path::Path,
    ) -> Result<PendingImage, String> {
        let pending = save_capture(bytes, session_dir)?;
        self.pending_images.push(pending.clone());
        Ok(pending)
    }

    /// The whole Ctrl+V face: the vision gate, the grab outcome, the chip or
    /// the error line. [`PasteOutcome::NoImage`] is the caller's cue to fall
    /// back to a plain text paste — everything else has been answered here.
    pub fn apply_pasted_image(
        &mut self,
        grabbed: Result<Option<Vec<u8>>, String>,
        session_dir: &std::path::Path,
    ) -> PasteOutcome {
        // Gate first: a model that cannot see pictures gets words and a
        // /model pointer — and no capture is written.
        if self.vision_sees_images() == Some(false) {
            self.set_notice("this model cannot see images - switch to a vision model with /model");
            return PasteOutcome::Rejected;
        }
        match grabbed {
            Ok(Some(bytes)) => match self.paste_image_bytes(&bytes, session_dir) {
                Ok(img) => {
                    self.set_notice(format!("📷 {}x{} attached", img.width, img.height));
                    PasteOutcome::Attached
                }
                Err(e) => {
                    self.push_error_line(&format!("paste failed: {e}"));
                    PasteOutcome::Rejected
                }
            },
            Ok(None) => PasteOutcome::NoImage,
            Err(e) => {
                // The helper's failure is shown, never swallowed into
                // "no image".
                self.push_error_line(&format!("clipboard image: {e}"));
                PasteOutcome::Rejected
            }
        }
    }

    /// Append a system/banner line (welcome, workspace, log path).
    pub fn push_system(&mut self, text: &str) {
        self.transcript.push_system(text);
    }

    /// Append startup-banner rows verbatim: one output line per row, **no**
    /// soft-wrap (the wordmark must stay whole; ratatui clips on narrow
    /// terminals). The rows arrive compiled (`BrandRow`): brand runs are
    /// literal colors, and the plain `text` keeps copy/selection style-blind.
    pub fn push_banner(&mut self, rows: Vec<BrandRow>) {
        for row in rows {
            self.transcript.push_styled_line(row.text, row.spans);
        }
    }

    /// The semantic color stylesheet run tokens resolve against at render time.
    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// Set the semantic color stylesheet (called once at TUI start).
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Set the product-semantic key bindings (startup wiring from `ShellSpec`).
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keymap = keymap;
    }

    /// Set the product wording table (startup wiring from `ShellSpec`).
    pub fn set_copy(&mut self, copy: CopyTable) {
        self.copy = copy;
    }

    /// Set the `@`/`/` popup band style (startup wiring from config).
    pub fn set_popup_style(&mut self, style: PopupStyle) {
        self.popup_style = style;
    }

    /// The `@`/`/` popup band style, with the selection highlight painted for
    /// the active scheme. The stored style only knows chrome (frame, width,
    /// height, separator) — its `highlight` is the widget default, and a
    /// default built for dark reads as no highlight at all on light. The
    /// background comes from `theme`; the foreground stays unset so rows keep
    /// their own two-tone foregrounds (the `/` popup's name/description split).
    pub fn popup_style(&self) -> PopupStyle {
        PopupStyle {
            highlight: Style::default()
                .bg(self.theme.selection_bg)
                .add_modifier(Modifier::BOLD),
            ..self.popup_style
        }
    }

    /// Update the output wrap width to match the terminal's content area.
    /// Called each frame from `draw()` so output and composer widths stay
    /// aligned. When the width changes, committed output lines are re-wrapped.
    pub fn set_wrap_width(&mut self, width: usize) {
        let width = width.max(1);
        if width == self.transcript.wrap_width() {
            return;
        }
        self.transcript.set_wrap_width(width);
        // Always rebuild the tail cache at the new width so future streaming
        // text wraps correctly, even when there is no pending text right now.
        self.stream.rewrap(width);
        for child in self.child_streams.values_mut() {
            child.rewrap(width);
        }
    }

    /// Current output wrap width (columns).
    #[allow(dead_code)]
    pub fn current_wrap_width(&self) -> usize {
        self.transcript.wrap_width()
    }

    /// Echo the user's submitted message into the transcript, so the frame log
    /// and output buffer retain the human side of the conversation (the JSONL
    /// turn log already stores `user_input`, but the rendered transcript didn't
    /// show it). `❯` on the first line, indented continuations after.
    pub fn push_user(&mut self, text: &str) {
        self.transcript.push_user(text);
    }
}

#[cfg(test)]
#[path = "../app_tests/mod.rs"]
mod app_tests;
