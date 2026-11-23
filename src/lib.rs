//! A reusable terminal-app shell for LLM agent products.
//!
//! This crate is the product shell of the phi family's TUI line: the event
//! loop and input pump, the transcript translation and rendering, the theme
//! and hit-testing protocol, the approval/ask interaction machinery, the deep
//! panel/picker skeletons, and the `ShellSpec`/`ShellHandles` wire-up types.
//!
//! **No product strategy lives here.** The charter split (mechanism in the
//! shell, wording/policy in the product) is enforced at the seams: everything
//! a product owns — command semantics, approval grading tables, risk copy,
//! data-directory layout, update/tips distribution, brand wording — enters
//! through `ShellSpec` data or `ShellHandles` fn slots. Defaults reproduce the
//! reference product's (phimint) voice so an unfilled spec still runs.
//!
//! **Shallow-tier timing invariant.** Products synthesize turns — click
//! answers, background-task wakes, child-report batches (design §4.2's shallow
//! tier). A synthetic turn is honored only when no turn is active, and lands
//! as a real user turn at the boundary — it is **never injected into a running
//! turn** (that silently pollutes the live turn's context). The rule is stated
//! on [`shell::Synthetic`] and pinned at screen level by the reference
//! product's PTY smoke gate (phimint `scripts/pty_smoke/`, scenario 10).
//!
//! Entry point: [`run::run`] with a [`shell::ShellSpec`] (data config) and
//! [`shell::ShellHandles`] (live objects). The product assembles its own
//! [`phi_agent::PhiAgent`] and hands it in — the shell never builds an agent.

pub mod app;
pub mod approval;
pub mod bg_wake;
pub mod child_results;
pub mod clipboard_image;
pub mod exit_dump;
pub mod frame_log;
pub mod handlers;
mod input_history;
pub mod interact_slot;
pub mod interaction;
pub mod picker;
pub mod render;
pub mod run;
mod scroll_guard;
pub mod session_state;
pub mod shell;
pub mod task_focus;
pub mod task_layout;
pub mod task_panel;
pub mod task_view;
#[cfg(unix)]
pub mod term_guard;
pub mod theme;
pub mod tips;

pub use app::PendingImage;
pub use approval::{ApprovalItem, ApprovalModeSwitch, RuntimeApprovalMode};
pub use exit_dump::{ExitDumpMode, SessionFacts};
pub use interact_slot::{CancelSource, InteractSlot};
pub use interaction::{
    AskAnswer, AskItem, AskOption, LiveInteractionChannel, PickAnswer, PickItem, PickRow,
};
pub use run::run;
pub use shell::{
    ApprovalCopy, CommandAction, CommandFuture, CommandSpec, ConsentProbe, CopyFacts, CopyMode,
    CopyNums, CopyOne, CopyPair, CopyRisk, CopyTable, CurrentSession, FrameLogPaths, Keymap,
    NoticeProbe, PanelTick, PromptFallbackFn, PromptSource, ReopenHook, RouteOutcome,
    SessionArchivedFn, SessionBoot, SessionBootFn, SessionEndFn, SessionEnumerateFn, SessionIo,
    SessionRestoreFn, SessionRow, ShellFlow, ShellHandles, ShellSpec, SkillBridge, SkillRebakeFn,
    SkillRouteFn, StatusFace, SubmitHook, Synthetic, TaskFuture, TickFacts, TickOut, TipsFetchFn,
    TipsSlot, TitleGenerator, TitleSlot, TitleWriteFn, UpdateCheckFn, UpdateSlot, VisionProbe,
    Wordmark, WordmarkBuilder,
};
pub use theme::{BrandRow, ColorScheme, ShellToken, SpanSpec, Theme};
pub use tips::{TipItem, TipKind};
