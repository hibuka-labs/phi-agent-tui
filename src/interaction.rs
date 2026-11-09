//! The [`InteractionChannel`] of the shell: consent rides the product's
//! approval gate, ask rides the picker queue.
//!
//! Mechanism half of the interaction seam: suspension discipline (answer /
//! cancel / detached-queue guard) is single-sourced here and every mounted
//! channel ([`crate::InteractSlot`]) delegates to it. The gate is an opaque
//! [`ApprovalHandler`] — grading tables, risk copy and the ledger are the
//! product's; the shell only carries one cancel token (the run's) shared by
//! both halves: a stopped turn releases both.
//!
//! Human time is unbounded in this shell (a picker may sit unanswered for
//! hours), so [`LiveInteractionChannel::exec_ctx`] clears the library watchdog
//! and leans on the run's cancel token alone.

use std::sync::Arc;

use async_trait::async_trait;
use phi_agent::{
    AgentError, ApprovalDecision, ApprovalHandler, ApprovalRequest, ExecCtx, InteractionChannel,
    InteractionError,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

// Ask vocabulary: the item the picker queue carries. `AskAnswer`/`AskOption`
// are engine types, re-exported so the picker and the product name one module
// for the whole ask surface.
pub use phi_agent::{AskAnswer, AskOption};

/// One pending ask handed to the picker, carrying the channel the user's
/// answer returns on.
#[derive(Debug)]
pub struct AskItem {
    /// The question to render.
    pub question: String,
    /// The options to pick from (may be empty — free-form yes/no).
    pub options: Vec<AskOption>,
    /// Complete this to deliver the user's answer back to the caller.
    pub answer_tx: oneshot::Sender<AskAnswer>,
}

// Pick vocabulary: the product-pushed list picker (the `/model` chooser).
// Same queue shape as asks — the item carries the channel the answer returns
// on — but the modal owns a typed filter instead of a question.

/// One row of a product pick: the display face only (id, label, note). How
/// the product enumerates its candidates is its own policy; the shell only
/// renders rows and reports which one the user confirmed.
#[derive(Debug, Clone)]
pub struct PickRow {
    /// The answer key handed back on confirm (e.g. a model name).
    pub id: String,
    /// What the user reads (e.g. `claude-opus-5-5 [main]`).
    pub label: String,
    /// Optional note under the label (e.g. "current").
    pub note: Option<String>,
    /// The row to highlight (the product's "this is the current one" mark).
    pub current: bool,
}

/// The picker's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickAnswer {
    /// The user confirmed `id`. `literal` marks the typed-name fallback: the
    /// filter matched no row and Enter took the text as typed (the product's
    /// "set it anyway" escape hatch).
    Chosen { id: String, literal: bool },
    /// Esc / dismiss — nothing changes.
    Cancelled,
}

/// One pending pick handed to the picker, carrying the channel the answer
/// returns on.
#[derive(Debug)]
pub struct PickItem {
    /// The window title (product copy, per pick).
    pub title: String,
    /// The rows to choose from.
    pub rows: Vec<PickRow>,
    /// Complete this to deliver the user's answer back to the caller.
    pub answer_tx: oneshot::Sender<PickAnswer>,
}

/// The ExecCtx interaction channel over the live shell surfaces.
pub struct LiveInteractionChannel {
    gate: Arc<dyn ApprovalHandler>,
    ask_tx: mpsc::UnboundedSender<AskItem>,
}

impl LiveInteractionChannel {
    /// Wire to the product's approval gate and the picker queue
    /// (the `ask_user` tool's sender, or whatever builds [`AskItem`]s).
    pub fn new(gate: Arc<dyn ApprovalHandler>, ask_tx: mpsc::UnboundedSender<AskItem>) -> Self {
        Self { gate, ask_tx }
    }

    /// The shell's execution context: the run's cancel token, this channel,
    /// and no interaction watchdog — human time is unbounded here (the run's
    /// cancel token and the detached-queue error guard the dead cases).
    pub fn exec_ctx(
        cancel: CancellationToken,
        gate: Arc<dyn ApprovalHandler>,
        ask_tx: mpsc::UnboundedSender<AskItem>,
    ) -> ExecCtx {
        ExecCtx::new(cancel, Arc::new(Self::new(gate, ask_tx))).with_interact_timeout(None)
    }
}

#[async_trait]
impl InteractionChannel for LiveInteractionChannel {
    async fn request_consent(
        &self,
        request: ApprovalRequest,
        cancel: &CancellationToken,
    ) -> Result<ApprovalDecision, InteractionError> {
        // The gate is the approval throat: same popup, same ledger, product
        // policy — the shell just forwards and maps the error face.
        self.gate
            .approve(request, cancel.clone())
            .await
            .map_err(channel_error)
    }

    async fn ask_user(
        &self,
        question: &str,
        options: &[AskOption],
        cancel: &CancellationToken,
    ) -> Result<AskAnswer, InteractionError> {
        let (answer_tx, answer_rx) = oneshot::channel();
        let item = AskItem {
            question: question.to_string(),
            options: options.to_vec(),
            answer_tx,
        };
        if self.ask_tx.send(item).is_err() {
            // No picker attached (a detached run loop) — say so rather than hang.
            return Err(InteractionError::Channel(
                "no terminal picker is attached to this session".into(),
            ));
        }
        tokio::select! {
            biased;
            answer = answer_rx => answer.map_err(|_| {
                InteractionError::Channel("the picker closed without an answer".into())
            }),
            _ = cancel.cancelled() => Err(InteractionError::Cancelled),
        }
    }
}

/// The kernel's approval errors onto the channel's error face.
fn channel_error(e: AgentError) -> InteractionError {
    match e {
        AgentError::Cancelled => InteractionError::Cancelled,
        other => InteractionError::Channel(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_agent::{ApprovalItem, QueuedApprovalHandler, RiskLevel};

    // The interaction scenarios (consent / ask / interrupt) over the same
    // queue shapes the TUI drains — the gate fixture is the family's own
    // `QueuedApprovalHandler`, so the queue contract is asserted end to end.

    fn queued_gate() -> (
        Arc<dyn ApprovalHandler>,
        mpsc::UnboundedReceiver<ApprovalItem>,
    ) {
        let (queue_tx, queue_rx) = mpsc::unbounded_channel();
        (Arc::new(QueuedApprovalHandler::new(queue_tx)), queue_rx)
    }

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            title: "write_file".into(),
            message: "Write file: src/lib.rs".into(),
            action_key: None,
            risk_level: RiskLevel::Sensitive,
            raw: None,
            source: None,
        }
    }

    fn options() -> Vec<AskOption> {
        vec![
            AskOption {
                id: "search".into(),
                label: "搜索".into(),
                note: Some("先打通找文件".into()),
            },
            AskOption {
                id: "login".into(),
                label: "登录".into(),
                note: None,
            },
        ]
    }

    #[tokio::test]
    async fn consent_rides_the_live_approval_queue() {
        let (gate, mut queue_rx) = queued_gate();
        let (ask_tx, _ask_rx) = mpsc::unbounded_channel();
        let ctx = LiveInteractionChannel::exec_ctx(CancellationToken::new(), gate, ask_tx);

        let call = tokio::spawn(async move { ctx.request_consent(request()).await });
        let item = queue_rx
            .recv()
            .await
            .expect("the prompt reaches the TUI queue");
        assert_eq!(item.request.title, "write_file");
        item.decision_tx.send(ApprovalDecision::AllowOnce).unwrap();

        assert_eq!(call.await.unwrap().unwrap(), ApprovalDecision::AllowOnce);
    }

    #[tokio::test]
    async fn ask_rides_the_ask_user_picker() {
        let (gate, _queue_rx) = queued_gate();
        let (ask_tx, mut ask_rx) = mpsc::unbounded_channel();
        let ctx = LiveInteractionChannel::exec_ctx(CancellationToken::new(), gate, ask_tx);

        let call = tokio::spawn(async move { ctx.ask_user("先做哪个？", &options()).await });
        let item = ask_rx
            .recv()
            .await
            .expect("the question reaches the picker");
        assert_eq!(item.question, "先做哪个？");
        assert_eq!(item.options.len(), 2);
        assert_eq!(item.options[0].note.as_deref(), Some("先打通找文件"));
        item.answer_tx
            .send(AskAnswer::Picked {
                id: "login".into(),
                label: "登录".into(),
            })
            .unwrap();

        assert_eq!(
            call.await.unwrap().unwrap(),
            AskAnswer::Picked {
                id: "login".into(),
                label: "登录".into(),
            }
        );
    }

    #[tokio::test]
    async fn a_skipped_pick_is_an_answer_too() {
        let (gate, _queue_rx) = queued_gate();
        let (ask_tx, mut ask_rx) = mpsc::unbounded_channel();
        let ctx = LiveInteractionChannel::exec_ctx(CancellationToken::new(), gate, ask_tx);

        let call = tokio::spawn(async move { ctx.ask_user("做不做？", &options()).await });
        let item = ask_rx.recv().await.unwrap();
        item.answer_tx.send(AskAnswer::Skipped).unwrap();

        assert_eq!(call.await.unwrap().unwrap(), AskAnswer::Skipped);
    }

    #[tokio::test]
    async fn interrupt_releases_a_suspended_consent() {
        let (gate, mut queue_rx) = queued_gate();
        let (ask_tx, _ask_rx) = mpsc::unbounded_channel();
        let token = CancellationToken::new();
        let ctx = LiveInteractionChannel::exec_ctx(token.clone(), gate, ask_tx);

        let call = tokio::spawn(async move { ctx.request_consent(request()).await });
        let _item = queue_rx.recv().await.unwrap(); // prompt is up; nobody answers
        token.cancel();

        assert!(matches!(
            call.await.unwrap(),
            Err(InteractionError::Cancelled)
        ));
    }

    /// The detached case is the shell-side deadlock guard: no picker
    /// attached must fail fast, never suspend forever.
    #[tokio::test]
    async fn a_detached_picker_fails_instead_of_hanging() {
        let (gate, _queue_rx) = queued_gate();
        let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskItem>();
        drop(ask_rx);
        let ctx = LiveInteractionChannel::exec_ctx(CancellationToken::new(), gate, ask_tx);

        let err = ctx.ask_user("做不做？", &options()).await.unwrap_err();
        assert!(matches!(&err, InteractionError::Channel(_)), "{err}");
    }
}
