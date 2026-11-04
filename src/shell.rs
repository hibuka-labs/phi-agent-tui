//! The shell wire-up types: `run(spec, agent, handles)` (T2 seam cut).
//!
//! Two halves, per the O1 two-stage decision (design notes §5.1):
//!
//! - [`ShellSpec`] — data configuration the product fills at boot. Anything
//!   that can be a field is a field (decision 1A). Unfilled slots default to
//!   phimint's current style once the shell lands in its own crate.
//! - [`ShellHandles`] — runtime live objects handed in exactly once. They are
//!   born after boot wiring (channels, registries, session state), so they
//!   never pretend to be spec fields.
//!
//! The agent itself is the middle argument of `run` — the shell does not own
//! agent construction (tools registration and prompt assembly are product-core
//! code; see design notes §5).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use log_core::SinkHandle;
use phi_agent::{ApprovalRequest, ChatMessage, Tool};
use phi_kernel_tools::background_shell::BackgroundTaskRegistry;
use phi_tui::popup_list::PopupStyle;
use tokio::sync::mpsc;

use crate::approval::{ApprovalItem, ApprovalModeSwitch};
use crate::exit_dump::{ExitDumpMode, SessionFacts};
use crate::interaction::AskItem;
use crate::tips::TipItem;

pub use super::session_state::CurrentSession;
use super::theme::{BrandRow, Theme};

/// What a product command tells the shell to do with its input (design §5:
/// the product steers the shell through flow values, never by reaching into
/// shell state).
///
/// The vocabulary grows with the hooks that steer shell navigation (Quit /
/// PushView / SwitchSession… arrive with the BandSlot/ViewMode batches);
/// command answers need exactly these two.
pub enum ShellFlow {
    /// Answered locally: settle the text as a system message — the input
    /// never becomes a model turn.
    Answer(String),
    /// The name matched but the command declined the input: fall through to
    /// skill routing and a plain turn, exactly like a non-command input.
    Unhandled,
}

/// A shell-spawned product task's future. `T = ()` for fire-and-forget work
/// (title generation); value-carrying seams use the generic form
/// (`TaskFuture<Option<String>>` for the update check).
pub type TaskFuture<T = ()> = Pin<Box<dyn Future<Output = T> + Send>>;
/// The future behind a product command action.
pub type CommandFuture = Pin<Box<dyn Future<Output = ShellFlow> + Send>>;

/// Product slash command action: the raw submit line in, a [`ShellFlow`] out.
/// Async because a command may block on human confirmation (the turn pump
/// serializes commands — a plugin install waits for its confirm popup).
pub type CommandAction = Arc<dyn Fn(String) -> CommandFuture + Send + Sync>;

/// One product slash command (design §5: `commands: Vec<CommandSpec>` —
/// name + help + hook).
///
/// The hook is a fn field ([`CommandAction`]) — the seam shape shared with
/// [`Wordmark`] and [`TitleGenerator`] (T1 §5: a fn field, not a new trait;
/// the trait budget stays at BandSlot/ViewMode). The (name, help) row also
/// feeds the `/` picker (when [`Self::in_picker`]), and the name is the
/// dispatch key (see [`Self::matches`]).
#[derive(Clone)]
pub struct CommandSpec {
    /// Command word without the trigger (`"model"` → `/model`).
    pub name: String,
    /// One-line help shown in the `/` picker (product copy).
    pub help: String,
    /// Whether the `/` picker lists this row. Listing is product wording:
    /// some commands are typed-only (phimint keeps its model-tier family out
    /// of the curated picker rows, pre-sink parity). Dispatch is unaffected —
    /// a hidden command still runs when typed.
    pub in_picker: bool,
    /// The action: raw submit line → [`ShellFlow`].
    pub action: CommandAction,
}

impl CommandSpec {
    /// A submit is this command's when its first word is exactly `/{name}` —
    /// `/tools off x` is `tools`, `/toolsx` is not (it falls through to the
    /// skill router and the model, as it always has).
    pub fn matches(&self, input: &str) -> bool {
        let Some(rest) = input.trim().strip_prefix('/') else {
            return false;
        };
        rest.split_whitespace().next() == Some(self.name.as_str())
    }
}

/// The `/` picker's data face: skill rows first, then the built-in rows, then
/// the product command rows that opt in ([`CommandSpec::in_picker`]). Rows are
/// (name, help) product copy; order is part of the surface.
pub fn slash_picker_rows(
    skills: Vec<(String, String)>,
    builtins: Vec<(String, String)>,
    commands: &[CommandSpec],
) -> Vec<(String, String)> {
    let mut rows = skills;
    rows.extend(builtins);
    rows.extend(
        commands
            .iter()
            .filter(|cmd| cmd.in_picker)
            .map(|cmd| (cmd.name.clone(), cmd.help.clone())),
    );
    rows
}

/// Panic isolation for product hooks (T12, §10): a product closure that
/// panics degrades to `fallback` and reports its name — the event loop must
/// never die because product code panicked. The `Some(name)` half is the
/// degradation's identity; the caller leaves the trace
/// ([`FrameLogs::degrade`](crate::frame_log::FrameLogs::degrade)) so the
/// downgrade is never silent.
pub(crate) fn hook_guard<T>(
    name: &'static str,
    f: impl FnOnce() -> T,
    fallback: T,
) -> (T, Option<&'static str>) {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => (value, None),
        Err(_) => {
            tracing::error!(hook = name, "product hook panicked; degraded to fallback");
            (fallback, Some(name))
        }
    }
}

/// Panic isolation for async product hooks (T12): either phase may panic —
/// the sync call building the future, or the future itself. The future runs
/// on its own task, so a panic there is a `JoinError`, not an unwind through
/// the loop.
pub(crate) async fn hook_guard_async<T: Send + 'static>(
    name: &'static str,
    make: impl FnOnce() -> TaskFuture<T> + Send + 'static,
    fallback: T,
) -> (T, Option<&'static str>) {
    let fut = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(make)) {
        Ok(fut) => fut,
        Err(_) => {
            tracing::error!(hook = name, "product hook panicked on call; degraded");
            return (fallback, Some(name));
        }
    };
    match tokio::spawn(fut).await {
        Ok(value) => (value, None),
        Err(_) => {
            tracing::error!(hook = name, "product hook future panicked; degraded");
            (fallback, Some(name))
        }
    }
}

/// Product session-title generator: `(conversation, session dir)` → the task
/// the shell spawns (T1 §5 fn-field seam).
///
/// The shell owns the trigger — the 10th user turn of a session — and the
/// fire-and-forget spawn; which model generates the title, the wording and
/// the persistence are product strategy.
pub type TitleGenerator = Arc<dyn Fn(Vec<ChatMessage>, PathBuf) -> TaskFuture + Send + Sync>;

/// The quit half of [`TitleSlot`]: write the pending-title marker
/// (`(sessions dir, session dir)`). The layout under the data root is product
/// policy.
pub type TitleWriteFn = Arc<dyn Fn(&Path, &Path) + Send + Sync>;

/// Product-semantic key bindings (design §5.2).
///
/// The configurable surface is deliberately narrow: picker triggers and
/// product command bindings. The fixed interaction protocol — Ctrl+C
/// interrupt discipline, Ctrl+L redraw, Esc layer close, Enter semantics,
/// cursor motion, terminal query replies (DSR/OSC) — is **not** here and
/// never will be: those are mechanism correctness, and "configurable" would
/// mean "configurable wrong" (§5.2; each fixed key gets a PTY smoke
/// assertion instead).
///
/// Values are the modifier-free key char; the modifier each action uses is
/// part of the binding's meaning (triggers are bare, command bindings are
/// Ctrl-chords). Defaults reproduce phimint's current keymap exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keymap {
    /// Char that opens the `/` skill picker at line start (default `/`).
    pub slash_trigger: char,
    /// Char that opens the `@` file picker (default `@`).
    pub mention_trigger: char,
    /// Ctrl-chord that quits (default `d`).
    pub quit: char,
    /// Ctrl-chord that copies the last reply (default `y`).
    pub copy_last_reply: char,
    /// Ctrl-chord that toggles thought-block display (default `o`).
    pub toggle_thoughts: char,
    /// Ctrl-chord that cycles the tool-preview ladder (default `e`).
    pub cycle_result_tier: char,
    /// Ctrl-chord that reopens the product panel (design canvas; default `b`).
    pub canvas_reopen: char,
    /// Ctrl-chord that toggles the task panel frame (default `t`).
    pub task_panel_toggle: char,
}

impl Default for Keymap {
    /// phimint's keymap — what an unfilled product gets.
    fn default() -> Self {
        Self {
            slash_trigger: '/',
            mention_trigger: '@',
            quit: 'd',
            copy_last_reply: 'y',
            toggle_thoughts: 'o',
            cycle_result_tier: 'e',
            canvas_reopen: 'b',
            task_panel_toggle: 't',
        }
    }
}

/// File names of the session's frame-log instrument trio (T11, §7.4:
/// "FrameCapture / PerfLog / ComposerLog 随 ShellSpec.paths"). Placement
/// under the session directory is product layout policy (charter); the
/// defaults are phimint's names. [`ShellSpec::paths`] is `Option` — `None`
/// turns the whole trio off (the logs are one instrument: the composer and
/// perf rows carry the capture's frame id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameLogPaths {
    /// Offscreen flipbook (default `frames.txt`).
    pub frames: String,
    /// Per-frame perf CSV (default `perf.log`).
    pub perf: String,
    /// Composer visual-state log (default `composer.log`).
    pub composer: String,
}

impl Default for FrameLogPaths {
    /// phimint's layout — what an unfilled product gets.
    fn default() -> Self {
        Self {
            frames: "frames.txt".to_string(),
            perf: "perf.log".to_string(),
            composer: "composer.log".to_string(),
        }
    }
}

/// Two-argument product wording formatter.
pub type CopyPair = Arc<dyn Fn(&str, &str) -> String + Send + Sync>;
/// Numeric product wording formatter (e.g. scroll position).
pub type CopyNums = Arc<dyn Fn(usize, usize) -> String + Send + Sync>;
/// Session-fact product wording formatter (exit dump).
pub type CopyFacts = Arc<dyn Fn(&SessionFacts<'_>) -> String + Send + Sync>;
/// Single-argument product wording formatter (e.g. a sub-agent name).
pub type CopyOne = Arc<dyn Fn(&str) -> String + Send + Sync>;
/// Risk-level product wording formatter (the whole risk line).
pub type CopyRisk = Arc<dyn Fn(&phi_agent::RiskLevel) -> String + Send + Sync>;
/// Approval-mode product wording formatter (badge / switch notice).
pub type CopyMode = Arc<dyn Fn(&crate::approval::RuntimeApprovalMode) -> String + Send + Sync>;

/// Approval/consent product wording (T14): the popups' words, the risk line,
/// the mode badge and its switch notice. The interaction state machine (queue,
/// popup, timeout, Esc/Enter discipline) is shell mechanism; **which words it
/// wears is product policy** — same slot rule as [`CopyTable`]: static copy is
/// a `String`, wording that takes runtime arguments is a formatter fn field.
///
/// The option labels carry their key chords (`"[y] allow once"`) because which
/// key picks which decision is product semantics (the shell only tracks the
/// selected [`phi_agent::ApprovalDecision`]). Colors stay shell: severity is visual
/// mechanism, keyed on the risk level / mode.
#[derive(Clone)]
pub struct ApprovalCopy {
    /// Popup head line.
    pub title: String,
    /// Sub-agent source marker line: `(source name)`.
    pub requested_by: CopyOne,
    /// The full risk line: `(risk level)` -> e.g. "Risk: Destructive".
    pub risk_line: CopyRisk,
    /// Tool-approval legend options (key chords included).
    pub allow_once: String,
    pub allow_always: String,
    pub deny: String,
    /// Consent-gate legend options (no standing consent, so no allow-always).
    pub consent_open: String,
    pub consent_skip: String,
    /// The commit-key label shared by both legends.
    pub confirm: String,
    /// Status-bar mode badge: `(mode)` -> e.g. "ask mode on".
    pub mode_badge: CopyMode,
    /// Mode-switch notice pushed on Shift+Tab: `(mode)`.
    pub mode_notice: CopyMode,
}

impl Default for ApprovalCopy {
    /// phimint's wording — what an unfilled product gets.
    fn default() -> Self {
        Self {
            title: "!! Approval required".to_string(),
            requested_by: Arc::new(|source| format!("requested by sub-agent [{source}]")),
            risk_line: Arc::new(|level| {
                let word = match level {
                    phi_agent::RiskLevel::Safe => "Safe",
                    phi_agent::RiskLevel::Sensitive => "Sensitive",
                    phi_agent::RiskLevel::Destructive => "Destructive",
                };
                format!("Risk: {word}")
            }),
            allow_once: "[y] allow once".to_string(),
            allow_always: "[a] allow always".to_string(),
            deny: "[n] deny".to_string(),
            consent_open: "[y] open browser".to_string(),
            consent_skip: "[n] skip".to_string(),
            confirm: "[Enter] confirm".to_string(),
            mode_badge: Arc::new(|mode| {
                match mode {
                    crate::approval::RuntimeApprovalMode::Auto => "auto mode on",
                    crate::approval::RuntimeApprovalMode::Ask => "ask mode on",
                    crate::approval::RuntimeApprovalMode::Deny => "deny mode on",
                }
                .to_string()
            }),
            mode_notice: Arc::new(|mode| {
                match mode {
                    crate::approval::RuntimeApprovalMode::Auto => "Mode: auto - Shift+Tab toggles",
                    crate::approval::RuntimeApprovalMode::Ask => "Mode: ask - Shift+Tab toggles",
                    crate::approval::RuntimeApprovalMode::Deny => {
                        "Mode: deny - Shift+Tab leaves deny"
                    }
                }
                .to_string()
            }),
        }
    }
}

/// Product wording for the shell's fixed chrome (design §5: "文案槽").
///
/// The slot rule mirrors the rest of the spec: **static copy is a `String`,
/// wording that takes runtime arguments is a formatter fn field** — the seam
/// shape shared with [`Wordmark`] and the title-generation slot (data where
/// it can be, fn fields where the words take arguments).
///
/// Approval/consent wording rides [`ApprovalCopy`] in the `approval` slot
/// (the T14 mechanism/policy split: risk labels, popups, mode notices).
/// Defaults reproduce phimint's current wording — an unfilled product gets
/// phimint's voice (design goal 1, same model as [`Keymap`]).
/// `Clone` (via `Arc` closures) because the shell re-creates its `App` on
/// every session switch and re-attaches the table each time.
#[derive(Clone)]
pub struct CopyTable {
    /// Brand stand-in shown when there is no wordmark: `(name, version)`.
    pub brand_fallback: CopyPair,
    /// Hint right behind the approval-mode badge in the status bar.
    pub mode_cycle_hint: String,
    /// Discoverability hint appended to a folded thought summary.
    pub thought_expand_hint: String,
    /// Scroll-position readout in the status bar: `(first_shown, total)`.
    pub scroll_hint: CopyNums,
    /// Exit-dump session header line.
    pub exit_dump_header: CopyFacts,
    /// Exit-dump trailer line.
    pub exit_dump_footer: CopyFacts,
    /// A session-skill bake failure, warned mid-turn: `(skill, error)`.
    pub skill_bake_failed: CopyPair,
    /// Approval/consent popups, risk line, mode badge and switch notice.
    pub approval: ApprovalCopy,
}

impl std::fmt::Debug for CopyTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Formatter slots are closures — print them as opaque, show the copy.
        f.debug_struct("CopyTable")
            .field("brand_fallback", &"<formatter>")
            .field("mode_cycle_hint", &self.mode_cycle_hint)
            .field("thought_expand_hint", &self.thought_expand_hint)
            .field("scroll_hint", &"<formatter>")
            .field("exit_dump_header", &"<formatter>")
            .field("exit_dump_footer", &"<formatter>")
            .field("skill_bake_failed", &"<formatter>")
            .field("approval", &"<wording slots>")
            .finish()
    }
}

impl Default for CopyTable {
    /// phimint's wording — what an unfilled product gets.
    fn default() -> Self {
        Self {
            brand_fallback: Arc::new(|name, version| {
                // ASCII `-`, not `—`: the CJK chrome guard
                // (`chrome_sources_stay_cjk_width_safe`) bans the em dash.
                format!("{name} v{version} - Forged with intent. Shipped with care.")
            }),
            mode_cycle_hint: "(shift+tab to cycle)".to_string(),
            thought_expand_hint: " (Ctrl+O expand)".to_string(),
            scroll_hint: Arc::new(|start, total| format!("scroll {start}/{total} (End to bottom)")),
            exit_dump_header: Arc::new(|facts| {
                let turns = facts.turns;
                format!(
                    "── phimint {} · session {} · {} {} · {} tok ──",
                    facts.version,
                    facts.session_id,
                    turns,
                    if turns == 1 { "turn" } else { "turns" },
                    crate::app::fmt_k(facts.tokens_in.saturating_add(facts.tokens_out)),
                )
            }),
            exit_dump_footer: Arc::new(|facts| {
                format!(
                    "── full log {}/messages.jsonl · phimint --resume to continue ──",
                    facts.session_dir.display()
                )
            }),
            skill_bake_failed: Arc::new(|skill, error| {
                format!("! skill `{skill}` could not be activated (prompt bake failed): {error}")
            }),
            approval: ApprovalCopy::default(),
        }
    }
}

/// The brand wordmark block, generated product-side (design §4.3: GLYPHS,
/// slogan and gradient ramp stay product brand code — the wordmark generator
/// is not shell mechanism).
///
/// The shell asks for rows with the current session's log path and the real
/// terminal width: the banner re-renders on every session switch, so those
/// two are runtime facts the generator receives rather than captures. The
/// width-sensitive lockup vs stacked layout is generator behavior, so it
/// crosses the seam as a field holding a function (the same shape as the
/// title-generation seam, T1 §5 — a fn field, not a new trait).
///
/// The rows arrive compiled (design §4.3, Issue 2.2): the generator flattens
/// its brand roles into literal-color runs — a narrow inline-style channel,
/// brand data only — so the shell never learns brand semantics.
/// Signature of a product wordmark generator: `(log_path, terminal width)` →
/// brand rows. A fn field, not a new trait (T1 §5 seam shape).
pub type WordmarkBuilder = Box<dyn Fn(&std::path::Path, usize) -> Vec<BrandRow> + Send>;

pub struct Wordmark {
    /// Build the banner rows: `(log_path, terminal width in columns)`.
    pub build: WordmarkBuilder,
}

// ── Behavior seams (design §7: product fn slots) ────────────────────────────
//
// Every cross-seam capability is a fn field or a live-object handle (T1 §5 —
// no new traits). Each is data in, data out: the product owns the policy and
// the wording, the shell owns the schedule and the send site.

/// The generic band face of a product panel's status row (text + severity).
/// The tick seam returns one per frame; `alert` flips the row to the warning
/// treatment. Wording is the product's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusFace {
    pub text: String,
    pub alert: bool,
}

/// One update check: `Some(hint)` when an upgrade is available, with the hint
/// line fully composed product-side (channel-aware command and all).
pub type UpdateCheckFn = Arc<dyn Fn() -> TaskFuture<Option<String>> + Send + Sync>;

/// Update checking (design §3). The shell owns the schedule — the boot
/// auto-check and the manual `/upgrade` intercept — and parks the composed
/// hint in the status bar; endpoints, install-source detection and the hint
/// wording are product data behind `check`.
#[derive(Clone)]
pub struct UpdateSlot {
    /// The check itself. `Some(hint)` = show it; `None`/error = silent.
    pub check: UpdateCheckFn,
    /// Run `check` once at boot. The manual `/upgrade` path runs it
    /// unconditionally.
    pub auto_check: bool,
    /// One-line help for the `/upgrade` row in the `/` picker (product copy).
    pub help: String,
}

/// One tips refresh pass: `Some(items)` when the manifest version changed.
pub type TipsFetchFn = Arc<dyn Fn() -> TaskFuture<Option<Vec<TipItem>>> + Send + Sync>;

/// Tips band content (design §3). The shell owns the re-check cadence; the
/// initial rows and the fetch/merge/cache policy are the product's.
#[derive(Clone)]
pub struct TipsSlot {
    /// Rows shown from the first frame (cache or built-in defaults).
    pub initial: Vec<TipItem>,
    /// The background fetch pass. Silent when `None`.
    pub fetch: TipsFetchFn,
}

/// Per-tick product notices: full system lines to insert into the transcript
/// (prefix and wording included). Called on every idle tick; the product keeps
/// its own "already announced" state behind the closure.
pub type NoticeProbe = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Consent-gate classification: `true` when an approval request is really the
/// product's consent gate (e.g. a panel launch-once question) rather than a
/// tool approval. Consent prompts wear the two-key legend and their `a` key
/// aliases the one-shot grant — standing consent is not offered, because
/// nothing would honor it. The classification and its wording are product
/// policy (charter: no product policy in the shell); the shell only asks.
pub type ConsentProbe = Arc<dyn Fn(&ApprovalRequest) -> bool + Send + Sync>;

/// One session-list row for the picker — the display face only; how the
/// product enumerates and scopes its sessions is its own policy, precomputed
/// into `in_workspace`.
pub struct SessionRow {
    /// The session's directory (`Enter` returns this).
    pub session_dir: PathBuf,
    /// Title shown on the row.
    pub title: String,
    /// Last activity, rendered as relative time.
    pub last_active_at: SystemTime,
    /// In the product's default (this-folder) view — the scope policy folded
    /// into one bit, so the picker's `Tab` toggle is pure display.
    pub in_workspace: bool,
}

/// Enumerate resumable sessions: `(data root, current session id) → rows`.
/// The current session is excluded by the product (don't list yourself).
pub type SessionEnumerateFn = Arc<dyn Fn(&Path, Option<&str>) -> Vec<SessionRow> + Send + Sync>;
/// Load a session's archived windows for the resume replay:
/// `(data root, session id) → messages`.
pub type SessionArchivedFn = Arc<dyn Fn(&Path, &str) -> Vec<ChatMessage> + Send + Sync>;
/// Heal a session's live window from its archive before a switch:
/// `(data root, session id, session dir) → healed`.
pub type SessionRestoreFn = Arc<dyn Fn(&Path, &str, &Path) -> bool + Send + Sync>;

/// Session persistence (design §3, T13): the skeleton and rendering are the
/// shell's, the storage format is the product's.
#[derive(Clone)]
pub struct SessionIo {
    pub enumerate: SessionEnumerateFn,
    pub archived: SessionArchivedFn,
    pub restore: SessionRestoreFn,
    /// One-line help for the `/resume` row in the `/` picker (product copy).
    pub resume_help: String,
}

/// The system-prompt fallback: used only when the agent carries no prompt of
/// its own. Everything else about prompt assembly is product-core.
pub type PromptFallbackFn = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone)]
pub struct PromptSource {
    pub fallback: PromptFallbackFn,
}

/// What a `/`-leading input becomes after skill routing (skill-lifetime v3;
/// command semantics are product policy).
pub enum RouteOutcome {
    /// A plain turn — no skill, or one already baked into the prompt.
    Turn { input: String },
    /// Turn-scope skill: the body rides as an ephemeral user message.
    Ephemeral { input: String },
    /// Session-scope skill: the product baked the body into `prompt`; the
    /// shell sets it and runs `input` (the raw command).
    SessionBaked {
        input: String,
        /// The full new system prompt (pristine base + active skills).
        prompt: String,
        /// The skill's canonical name — for the bake-failure warning.
        skill: String,
        /// Product-composed warnings surfaced before the turn (e.g. a
        /// persistence failure).
        warnings: Vec<String>,
    },
}

/// Route one input through the product's skill table.
pub type SkillRouteFn = Arc<dyn Fn(&str) -> RouteOutcome + Send + Sync>;
/// Re-bake the active-skill prompt on resume: pristine base in, the full new
/// prompt out (`None` = nothing active, leave the prompt alone).
pub type SkillRebakeFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The product's skill surface (design §3). `None` in [`ShellSpec::skills`] =
/// no skill routing: every input is a plain turn.
#[derive(Clone)]
pub struct SkillBridge {
    pub rebake: SkillRebakeFn,
    pub route: SkillRouteFn,
    /// The `/` picker's skill rows: `(name, help)` pairs (product copy).
    pub summaries: Arc<dyn Fn() -> Vec<(String, String)> + Send + Sync>,
    /// Pre-turn telemetry custom (skill usage snapshot for the turn record).
    pub turn_custom: Arc<dyn Fn() -> Option<serde_json::Value> + Send + Sync>,
    /// Post-turn telemetry custom (session-level skill usage).
    pub session_custom: Arc<dyn Fn() -> serde_json::Value + Send + Sync>,
}

/// Session-title generation, both halves (T5d):
#[derive(Clone)]
pub struct TitleSlot {
    /// The generator: `(conversation, session dir)` → the spawned task.
    pub generate: TitleGenerator,
    /// Write the pending-title marker at quit: `(sessions dir, session dir)`.
    /// The layout under the data root is product policy.
    pub write_marker: TitleWriteFn,
}

/// Facts the shell hands the product panel's tick closure each loop iteration.
pub struct TickFacts {
    /// A turn is in flight or queued (the busy face).
    pub running: bool,
    /// The composer holds text — a click answer must not steal the submit.
    pub composer_holds: bool,
    /// The root turn fell this tick (the falling edge).
    pub turn_fell: bool,
    /// This tick's timestamp (coalesce windows count from it).
    pub now: Instant,
}

/// A synthetic turn the product wants injected at the boundary (shallow-tier
/// event synthesis, design §4.2: click answers, background-task wakes,
/// child-report batches).
///
/// **Timing invariant — a synthetic turn is honored only when no turn is
/// active; it is never injected into a running turn.** Mid-turn injection
/// silently pollutes the live turn's context (the fan-in injection bug class:
/// results absorbed mid-turn leave no trace and muddle the answer); instead
/// the injection waits and lands as a real user turn at the boundary. The
/// shell enforces the root-turn floor — a [`TickOut::synthetic`] offered while
/// the root turn runs is dropped, and the panel re-offers from its own queue
/// on a later tick ([`TickFacts::running`] tells it when). Delivery policies
/// that also hold through child waits and background jobs layer their own
/// gates on top (see `bg_wake`'s hold and `child_results`' batch routing).
/// The invariant is pinned at screen level by the reference product's PTY
/// smoke gate (phimint `scripts/pty_smoke/`, scenario 10).
pub struct Synthetic {
    /// The user-message text (committed like a real submit).
    pub text: String,
    /// A real user answer (a click counts toward the title threshold); a
    /// generated report does not.
    pub user: bool,
}

/// What the product panel wants from one tick (design §3).
pub struct TickOut {
    /// Transcript lines to push (product voice, pushed before `synthetic`).
    pub notes: Vec<String>,
    /// A turn to inject — subject to [`Synthetic`]'s timing invariant (only
    /// honored when no turn is active, never dropped into a running one).
    pub synthetic: Option<Synthetic>,
    /// The status-band face. Assigned unconditionally, so `None` clears the
    /// face — a panel that wants to keep one returns it every tick.
    pub status: Option<StatusFace>,
    /// The panel changed something worth a redraw.
    pub dirty: bool,
}

/// The product panel's per-tick closure: subscriptions, lifecycle polls,
/// click drains, status face — all product state behind one fn.
pub type PanelTick = Arc<dyn Fn(&TickFacts) -> TickOut + Send + Sync>;

/// Enter-boundary rewrite: pending picks merge behind the typed text.
/// `None` = submit the input unchanged.
pub type SubmitHook = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// Reopen the product panel (Ctrl+B). Async because it may rebind a server;
/// awaited inline, then the shell redraws.
pub type ReopenHook = Arc<dyn Fn() -> TaskFuture<()> + Send + Sync>;

/// Per-session product boot: the tools to register and the queues to serve.
pub struct SessionBoot {
    /// Tools registered for this session (product core).
    pub tools: Vec<Arc<dyn Tool>>,
    /// The picker queue (the `ask_user` tool's receiver) — `None` = no asks.
    pub ask_rx: Option<mpsc::UnboundedReceiver<AskItem>>,
    /// The product panel's consent prompts — same popup face as approvals.
    pub consent_rx: Option<mpsc::UnboundedReceiver<ApprovalItem>>,
}

/// Run once per session iteration: `(session id, session dir) → boot`.
/// The id is the session's string identity (the log/tool-face path key), not
/// the engine's `SessionId` handle. Re-armed on every session switch (the
/// queue is per-session by design).
pub type SessionBootFn = Arc<dyn Fn(&str, &Path) -> TaskFuture<SessionBoot> + Send + Sync>;
/// Run when a session's UI lifecycle ends (quit or switch): graceful product
/// teardown. Awaited inline before the exit replay / next iteration.
pub type SessionEndFn = Arc<dyn Fn() -> TaskFuture<()> + Send + Sync>;

/// Data configuration for one shell run (O1 first half).
///
/// Strategy lives here as data; behavior lives in the agent, the handles, or
/// the product side. The shell must never grow product policy (charter:
/// no product strategy in the shell).
pub struct ShellSpec {
    /// Root of the product's data directory (e.g. `~/.phimint`).
    ///
    /// The layout *under* this root is product policy (charter), reached
    /// through the session hooks — the shell only holds the path.
    pub data_dir: PathBuf,
    /// Frame-log instrument placement (§7.4, T11): `Some` = the trio is on
    /// and lands under each session dir with these names; `None` = off.
    /// `Some(FrameLogPaths::default())` is phimint (the pre-sink default).
    pub paths: Option<FrameLogPaths>,
    /// Workspace root (status bar, hit protocol, brand banner).
    pub workspace: PathBuf,
    /// Product name — status bar, LSP client info, path hints (identity
    /// block with [`Self::version`]).
    pub name: &'static str,
    /// Product version — update check, banner, exit-dump facts.
    pub version: String,
    /// Brand wordmark banner. `None` = no wordmark; the product's fallback
    /// copy line shows instead (CopyTable slot once T5c lands).
    pub wordmark: Option<Wordmark>,
    /// Semantic color stylesheet ([`Theme`]): class-name tokens resolve
    /// against it at render time. `Theme::for_scheme(scheme)` is phimint's
    /// palette — every unfilled slot is phimint style, and a product recolors
    /// by overriding fields (struct-update syntax).
    pub theme: Theme,
    /// Completion popup style.
    pub popup_style: PopupStyle,
    /// Product-semantic key bindings (§5.2). `Keymap::default()` is phimint's
    /// keymap.
    pub keymap: Keymap,
    /// Product wording for the shell's chrome. `CopyTable::default()` is
    /// phimint's wording.
    pub copy: CopyTable,
    /// Product slash commands (design §5): dispatched locally on the turn
    /// pump before skill routing — a local answer never becomes a model
    /// turn. Their (name, help) rows also feed the `/` picker. Empty = the
    /// product has no local commands.
    pub commands: Vec<CommandSpec>,
    /// Product session-title generation ([`TitleSlot`]). `None` = the
    /// product has no session titles.
    pub title_gen: Option<TitleSlot>,
    /// Boot-time notices (e.g. socket-board load errors) the shell prints
    /// on the first frame — once per process, never replayed on a session
    /// switch.
    pub startup_notices: Vec<String>,
    /// Update checking ([`UpdateSlot`]). `None` = no update feature at all —
    /// `/upgrade` then has no row and no action.
    pub update: Option<UpdateSlot>,
    /// Tips band content ([`TipsSlot`]). `None` = off.
    pub tips: Option<TipsSlot>,
    /// Per-tick product notices ([`NoticeProbe`]). `None` = silent.
    pub notices: Option<NoticeProbe>,
    /// Consent-gate classification ([`ConsentProbe`]). `None` = every approval
    /// prompt wears the tool (three-key) face and `a` means AllowAlways.
    pub is_consent: Option<ConsentProbe>,
    /// Session persistence ([`SessionIo`]) — picker enumeration, the resume
    /// replay's archives, and the pre-switch heal. `None` = the product is
    /// not session-based (O6: 不填即关) — `/resume` then has no row and no
    /// action, and there is no picker or replay.
    pub session_io: Option<SessionIo>,
    /// System-prompt fallback ([`PromptSource`]).
    pub system_prompt: PromptSource,
    /// Skill routing ([`SkillBridge`]). `None` = every input is a plain turn.
    pub skills: Option<SkillBridge>,
    /// Product panel tick ([`PanelTick`]). `None` = no product panel.
    pub tick: Option<PanelTick>,
    /// Enter-boundary rewrite ([`SubmitHook`]). `None` = submit unchanged.
    pub on_submit: Option<SubmitHook>,
    /// Panel reopen key ([`ReopenHook`]). `None` = the key does nothing.
    pub on_reopen: Option<ReopenHook>,
    /// Exit transcript replay mode.
    pub exit_dump: ExitDumpMode,
}

/// Runtime live objects, handed in once through `run(spec, agent, handles)`
/// (O1 second half).
pub struct ShellHandles {
    /// Everything a `/resume` switch replaces wholesale.
    pub session_state: CurrentSession,
    /// Background shell task registry (the quit path shuts it down).
    pub bg_registry: Arc<BackgroundTaskRegistry>,
    /// Approval queue drained by the approval popup state machine.
    pub approval_rx: mpsc::UnboundedReceiver<ApprovalItem>,
    /// Approval mode switch (auto-approve etc.).
    pub approval_mode: Arc<ApprovalModeSwitch>,
    /// Log sinks; re-pointed at the resumed session's log file on switch.
    pub log_sinks: SinkHandle,
    /// Per-session product boot ([`SessionBootFn`]): tools + queues.
    pub session_boot: SessionBootFn,
    /// Per-session product teardown ([`SessionEndFn`]).
    pub session_end: SessionEndFn,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            help: String::new(),
            in_picker: true,
            action: Arc::new(|_| Box::pin(std::future::ready(ShellFlow::Unhandled))),
        }
    }

    #[test]
    fn command_matches_the_whole_first_word_only() {
        let s = spec("tools");
        assert!(s.matches("/tools"));
        assert!(s.matches("/tools off add_tool"));
        assert!(s.matches("  /tools  "));
        assert!(s.matches("/tools\tx"));
        // The name must be the whole first word, never a prefix of one —
        // `/toolsx` and `/tool` fall through to the skill router / a turn.
        assert!(!s.matches("/toolsx"));
        assert!(!s.matches("/tool"));
        assert!(!s.matches("tools"));
        assert!(!s.matches("/"));
        assert!(!s.matches(""));
        assert!(!s.matches("hello"));
        // A trailing argument that repeats the name is still this command.
        let m = spec("model");
        assert!(m.matches("/model model"));
        // ...but a longer command word is not (`/models` is its own spec).
        assert!(!m.matches("/models"));
    }

    #[test]
    fn picker_rows_skip_typed_only_commands() {
        // `in_picker: false` rows stay out of the `/` list but keep their
        // dispatch — the product's curated picker face is listing policy
        // only (phimint's model-tier family, pre-sink parity).
        let mut hidden = spec("model");
        hidden.help = "Show or set the main model".to_string();
        hidden.in_picker = false;
        let mut shown = spec("plugin");
        shown.help = "List, install, or remove plugins".to_string();
        let rows = slash_picker_rows(
            vec![("design-canvas".into(), "Design questions".into())],
            vec![("resume".into(), "Switch to another session".into())],
            &[hidden.clone(), shown],
        );
        assert_eq!(
            rows,
            vec![
                ("design-canvas".to_string(), "Design questions".to_string()),
                (
                    "resume".to_string(),
                    "Switch to another session".to_string()
                ),
                (
                    "plugin".to_string(),
                    "List, install, or remove plugins".to_string()
                ),
            ]
        );
        // The hidden row still dispatches when typed.
        assert!(hidden.matches("/model gpt-x"));
    }

    // ── T12 panic isolation ────────────────────────────────────────────────

    #[test]
    fn hook_guard_passes_the_value_through_on_success() {
        let (value, degraded) = hook_guard("t.ok", || 41 + 1, 0);
        assert_eq!(value, 42);
        assert!(degraded.is_none(), "no panic, no degradation");
    }

    #[test]
    fn hook_guard_degrades_to_the_fallback_and_names_the_hook() {
        let (value, degraded) = hook_guard("t.panicky", || panic!("product bug"), 7);
        assert_eq!(value, 7, "a panicking hook lands on its fallback");
        assert_eq!(
            degraded,
            Some("t.panicky"),
            "the degradation trace carries the hook's name"
        );
    }

    #[tokio::test]
    async fn hook_guard_async_passes_the_value_through() {
        let (value, degraded) = hook_guard_async("t.ok", || Box::pin(async { 3 }), 5).await;
        assert_eq!(value, 3);
        assert!(degraded.is_none());
    }

    #[tokio::test]
    async fn hook_guard_async_degrades_when_the_call_phase_panics() {
        // The sync half that builds the future is product code too.
        let (value, degraded) =
            hook_guard_async("t.call", || panic!("product bug in the call phase"), 5).await;
        assert_eq!(value, 5);
        assert_eq!(degraded, Some("t.call"));
    }

    #[tokio::test]
    async fn hook_guard_async_degrades_when_the_future_panics() {
        // A panic inside the future surfaces as a JoinError on its own task —
        // it must not unwind through the awaiting loop.
        let (value, degraded) = hook_guard_async(
            "t.future",
            || Box::pin(async { panic!("product bug in the future") }),
            5,
        )
        .await;
        assert_eq!(value, 5);
        assert_eq!(degraded, Some("t.future"));
    }

    // ── T14 approval wording seam ────────────────────────────────────────────

    #[test]
    fn approval_copy_defaults_reproduce_phimints_voice() {
        // The unfilled-product contract (design goal 1): Default is phimint's
        // wording, so this test IS the voice pin.
        let copy = ApprovalCopy::default();
        assert_eq!(copy.title, "!! Approval required");
        assert_eq!(copy.allow_once, "[y] allow once");
        assert_eq!(copy.allow_always, "[a] allow always");
        assert_eq!(copy.deny, "[n] deny");
        assert_eq!(copy.consent_open, "[y] open browser");
        assert_eq!(copy.consent_skip, "[n] skip");
        assert_eq!(copy.confirm, "[Enter] confirm");
        assert_eq!(
            (copy.requested_by)("worker-1"),
            "requested by sub-agent [worker-1]"
        );
        assert_eq!(
            (copy.risk_line)(&phi_agent::RiskLevel::Destructive),
            "Risk: Destructive"
        );
        use crate::approval::RuntimeApprovalMode as M;
        assert_eq!((copy.mode_badge)(&M::Auto), "auto mode on");
        assert_eq!((copy.mode_badge)(&M::Deny), "deny mode on");
        assert_eq!((copy.mode_notice)(&M::Ask), "Mode: ask - Shift+Tab toggles");
    }
}
