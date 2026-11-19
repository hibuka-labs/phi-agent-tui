# phi-agent-tui

[![Crates.io](https://img.shields.io/crates/v/phi-agent-tui.svg)](https://crates.io/crates/phi-agent-tui)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

A reusable terminal-app shell for LLM agent products, built on ratatui +
crossterm and the phi family's agent runtime. It gives a product the whole
"human talks to an agent in a terminal" application frame — event loop and
input pump, transcript translation and rendering, semantic theme,
approval/ask interaction machinery, session picker and resume skeleton, and
the `ShellSpec` / `ShellHandles` wire-up — while every product decision stays
on the product's side of the seam.

**No product strategy lives here.** Command semantics, approval grading
tables, risk wording, data-directory layout, brand copy, update/tips
distribution — all of it enters through `ShellSpec` data or `ShellHandles` fn
slots. Defaults reproduce the reference product's (phimint) voice so an
unfilled spec still runs. The product assembles its own
`phi_agent::PhiAgent` and hands it in; the shell never builds an agent.

## What's inside

| Module | What it gives you |
|---|---|
| `run` | The event loop: input pump, event drain, turn submission, synthetic-turn injection, update/tips/title hooks, size and raw-mode self-heals |
| `shell` | The wire-up: `ShellSpec` (data) + `ShellHandles` (live objects), wording slots (`CopyTable` / `ApprovalCopy`), `Keymap`, `CommandSpec`, `SessionIo`, panel/picker hooks |
| `app` | Conversation state: transcript commits, streaming tails, composer + queued steering, submit discipline (Esc/Enter duality), status faces |
| `render` | Frame composition and the hit-testing protocol (mouse → semantic hit), approval popup / picker / task-panel frames, status bar |
| `theme` | Semantic `Theme` (class-name tokens resolved at render time) + brand rows |
| `approval` | The approval state machine: queue, popup, timeout, Esc/Enter discipline, two-key consent gate — mechanism only; grading tables are product policy |
| `interaction` | Ask/consent vocabulary: `AskItem` / `AskAnswer` / `AskOption`, the picker queue, `LiveInteractionChannel` |
| `picker` | Deep picker skeleton (rows, filter, resume/reopen consumers) |
| `task_panel` | Deep multi-task panel skeleton (sub-agents + background shell tasks) |
| `bg_wake` / `child_results` | Synthetic-turn delivery policies: hold mid-turn, batch, quiet window, report-once |
| `frame_log` / `exit_dump` | The flipbook trio (frames / perf / exit dump) for post-hoc debugging |
| `term_guard` / `tips` | Unix termios self-heal against external clobber; rotating tip-band mechanics (content is product data) |

## Panel tiers

Products extend the UI at three depths (each with a live customer in phimint):

- **Shallow — event synthesis.** The product keeps its own panel and injects
  synthetic turns (click answers, background-task wakes, child-report
  batches).
- **Middle — layout slots.** Reserved bands (tips strip, status-band face)
  the product fills with data each tick.
- **Deep — view modes.** The shell owns focus and scroll switching; the
  product registers content (`task_panel`, pickers).

## Usage

```toml
[dependencies]
phi-agent-tui = "0.1"
```

```rust
use phi_agent_tui::shell::{ShellSpec, ShellHandles, CopyTable, Keymap, Theme};
use phi_agent_tui::theme::ColorScheme;

// Data side — every slot documents its default. `CopyTable::default()`,
// `Keymap::default()` and `Theme::for_scheme(..)` carry the reference
// product's voice (phimint): override only what your product phrases
// differently. Remaining slots (wordmark, title_gen, update, tips, notices,
// skills, tick, on_submit, on_reopen, exit_dump, ...) are optional fn slots —
// `None` = the feature is off.
let spec = ShellSpec {
    data_dir: home.join(".myagent"),
    workspace: std::env::current_dir()?,
    name: "myagent",
    version: env!("CARGO_PKG_VERSION").into(),
    theme: Theme::for_scheme(ColorScheme::Dark),
    copy: CopyTable::default(),
    keymap: Keymap::default(),
    commands: my_commands(),   // Vec<CommandSpec> — product semantics
    session_io: None,          // not session-based: /resume stays off
    // ...
};

// Live side — the product assembles its own agent and hands it in.
let handles = ShellHandles {
    session_state, bg_registry, approval_rx,
    approval_mode, log_sinks, session_boot, session_end,
};

run(spec, agent, handles).await?;
```

The reference consumer is **phimint**, the family's coding agent — its
`src/main.rs` + `src/commands.rs` are a full production wiring.

## Design notes

- **Mechanism vs. wording.** The shell owns mechanisms (event loop, state
  machines, colors, layout joins); the product owns wording and policy. The
  risk *color* is shell, the risk *word* is product; option labels carry key
  chords because key→decision mapping is product semantics.
- **Synthetic-turn timing invariant.** A synthetic turn is honored only when
  no turn is active and lands as a real user turn at the boundary — never
  injected into a running one (mid-turn injection silently pollutes the live
  turn's context). Stated on `shell::Synthetic`; pinned at screen level by
  the reference product's PTY smoke gate.
- **Hooks degrade, never crash.** Product fn slots run behind guards: a
  panicking hook falls back and leaves a trace in the flipbook instead of
  killing the event loop.
- **Fn fields, not traits.** The wire-up seam is `Arc<dyn Fn…>` slots — the
  family's established seam shape; there is no plugin trait to implement.

## Family

- [phi-agent](https://crates.io/crates/phi-agent) — the agent runtime this shell wraps
- [phi-tui](https://crates.io/crates/phi-tui) — chat-style TUI components (pure widgets, no agent semantics)
- [phi-kernel-tools](https://crates.io/crates/phi-kernel-tools) — kernel tools (file / shell / background tasks)
- [code-intel](https://crates.io/crates/code-intel) — language intelligence (LSP, repo map, ripgrep cores)
- [phimint](https://crates.io/crates/phimint) — the coding agent built on this shell

## License

MIT OR Apache-2.0 — see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
