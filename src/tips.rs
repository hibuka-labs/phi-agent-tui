//! The tips-band line vocabulary: what one tip row is.
//!
//! Data vocabulary rides with the shell (same judgment as `BrandRow`): the
//! renderer needs `kind` to label the row. Content, fetching, caching and the
//! manifest format stay product — the product hands the shell ready-made
//! [`TipItem`] rows through the `tips` seam.

use serde::{Deserialize, Serialize};

/// Whether an item is usage advice or a promotional note. Ads and promos must
/// use `Note`: the renderer labels them `Note:` and they never masquerade as
/// `Tip:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TipKind {
    /// Usage advice, rendered as `Tip:`.
    #[default]
    Tip,
    /// Promotional or informational note, rendered as `Note:`.
    Note,
}

impl TipKind {
    /// Render label shown to the user (`"Tip"` / `"Note"`).
    pub fn label(&self) -> &'static str {
        match self {
            TipKind::Tip => "Tip",
            TipKind::Note => "Note",
        }
    }
}

/// One tips line. `text` must be ASCII-safe for TUI chrome width.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipItem {
    /// Kind of item; missing fields default to `Tip`.
    #[serde(default)]
    pub kind: TipKind,
    /// The line's text (without the `Tip:`/`Note:` label).
    pub text: String,
}

impl TipItem {
    /// Build a `Tip` item.
    pub fn tip(text: impl Into<String>) -> Self {
        TipItem {
            kind: TipKind::Tip,
            text: text.into(),
        }
    }

    /// Build a `Note` item.
    pub fn note(text: impl Into<String>) -> Self {
        TipItem {
            kind: TipKind::Note,
            text: text.into(),
        }
    }
}
