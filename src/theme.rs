//! Color theme: semantic slots + the style vocabulary that resolves against
//! them, two static palettes (Dark/Light).
//!
//! Three kinds of color land on screen, and only the last two need a theme:
//!
//! 1. **ANSI semantic hues** (red / green / cyan / yellow) — the terminal's own
//!    palette paints them, so they already mean the same thing on light and
//!    dark backgrounds. Use sites keep `Color::Red` & co. directly.
//! 2. **ANSI grays** — their *role* flips with the background: the gray that
//!    reads "quiet hint" on dark reads "dead" on light. Two semantic slots,
//!    [`Theme::muted`] and [`Theme::faint`], map to different grays per scheme.
//! 3. **Absolute truecolor** (thought, user, warm, good, selection, error) —
//!    one RGB value cannot serve both backgrounds, so both sets live here and
//!    the scheme picks.
//!
//! Two static palettes, no dynamic adaptation: `--color-scheme dark|light|auto`
//! resolves once at startup (`banner::resolve_scheme`, keyed off `$COLORFGBG`)
//! and every slot is a pure function of that choice. The exact values are
//! aesthetic calls — when light reads wrong, tune them here and only here.
//!
//! The style language borrows CSS's layering model, not its syntax (design
//! §4.3, Issue 2.2):
//!
//! | CSS concept | here |
//! |---|---|
//! | class name (semantic, value-free) | [`ShellToken`] — a styled run stores the name, never a value |
//! | stylesheet (name → value) | [`Theme`] (this module, Dark/Light palettes) |
//! | inline style (literal value) | [`ShellToken::Literal`] — a narrow channel, brand data only |
//! | gradient (computed style) | the brand layer compiles its ramp into per-letter literal spans (terminals have no gradient primitive) |

use ratatui::style::{Color, Modifier, Style};

/// Which palette pair to use. `Dark` is the default (most terminals); `Light`
/// swaps every hue for a deeper, higher-contrast variant.
///
/// The selector is shell vocabulary, not brand (design §4.3): the product's
/// brand layer keys its own ramp off it too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    #[default]
    Dark,
    Light,
}

/// The stylesheet half of the layering: one value per semantic slot, with the
/// Dark/Light palettes built in (design §4.3 — "semantic-slot struct + Dark/Light
/// double panes, defaults = phimint's current values").
///
/// Fields are public so a product recolors by overriding fields (struct-update
/// syntax) instead of reimplementing a palette — `Theme { user:
/// Color::Magenta, ..Theme::for_scheme(scheme) }`. An unfilled slot is phimint
/// style end to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Readable but secondary text: tool arguments, result bodies, popup
    /// descriptions. Content — just not where the eye lands first.
    pub muted: Color,
    /// Status-band chrome: the Idle status word, the tip, the mode-cycle hint.
    /// A step brighter than `faint` so the bottom band reads without squinting;
    /// content chrome (timestamps, markers) stays at `faint`.
    pub hint: Color,
    /// The bottom-right status slot (scroll position, upgrade hint). A medium
    /// green pinned in truecolor: the call is for a shade ("not too dark, not
    /// too light") and ANSI Green is terminal-defined, landing anywhere from
    /// deep to neon. Content success markers keep `Color::Green`.
    pub good: Color,
    /// Chrome: borders, timestamps, markers, hint tails. Truecolor on both
    /// schemes: ANSI DarkGray is palette-defined and lands near-invisible on
    /// popular dark terminal themes (Apple Terminal's bright black is ~#666,
    /// unreadable on a navy bg), so Dark pins a mid gray; Light steps darker,
    /// not lighter, to stay visible on white.
    pub faint: Color,
    /// Process text (thinking). A truecolor mid-grey on dark, deliberately
    /// between ANSI Gray and DarkGray: DarkGray sits at notice-level on most
    /// dark themes (invisible), Gray reads as content. Thought is process —
    /// dimmer than results, still legible. On light the same role wants a
    /// mid-dark gray.
    pub thought: Color,
    /// Your own input in the transcript. ANSI Blue is navy on dark themes and
    /// near-black on light ones, so both sets are truecolor brights/deeps.
    pub user: Color,
    /// Brand warm: the mode badge. A lighter step of the logo's warm gradient —
    /// the gradient start `--warm-1` (`#ff6a3d`) reads dark and reddish at
    /// chrome text size, so the badge rides the banner's Slogan stop
    /// (`#ffb066`) on dark; Light keeps the hue but drops the red (`#e07a3c`)
    /// for white backgrounds.
    pub warm: Color,
    /// Foregrounds that must read on any background — popup row names on light.
    pub strong: Color,
    /// Selection highlight background (transcript rows, task panel, menus).
    pub selection_bg: Color,
    /// Selection highlight foreground, where a row has no opinion of its own. A
    /// `fg` forced over row spans would flatten two-tone rows (the `/` popup's
    /// name/description split), so most call sites apply `selection_bg` only.
    pub selection_fg: Color,
    /// A dim foreground stepped up (dark) / down (light) to survive sitting on
    /// `selection_bg` — a `faint` row that would vanish into the highlight.
    pub lifted: Color,
    /// The turn-final failure marker: a subdued warm dark red, so a verdict
    /// reads as a verdict and not as a live error line. The only warm slot
    /// outside the brand channel — same restraint as the banner palette.
    pub error: Color,
}

impl Theme {
    /// The Dark palette — phimint's current values.
    pub fn dark() -> Self {
        Self {
            muted: Color::Gray,
            hint: Color::Rgb(0xa0, 0xa0, 0xa0),
            good: Color::Rgb(0x66, 0xbb, 0x6a),
            faint: Color::Rgb(0x8a, 0x8a, 0x8a),
            thought: Color::Rgb(0x9e, 0x9e, 0x9e),
            user: Color::Rgb(0x5a, 0xa7, 0xf7),
            warm: Color::Rgb(0xff, 0xb0, 0x66),
            strong: Color::White,
            selection_bg: Color::DarkGray,
            selection_fg: Color::White,
            lifted: Color::Gray,
            error: Color::Rgb(0xc0, 0x40, 0x30),
        }
    }

    /// The Light palette — phimint's current values.
    pub fn light() -> Self {
        Self {
            muted: Color::Rgb(0x55, 0x55, 0x55),
            hint: Color::Rgb(0x5e, 0x5e, 0x5e),
            good: Color::Rgb(0x2e, 0x7d, 0x32),
            faint: Color::Rgb(0x6e, 0x6e, 0x6e),
            thought: Color::Rgb(0x6b, 0x6b, 0x6b),
            user: Color::Rgb(0x1a, 0x6f, 0xd4),
            warm: Color::Rgb(0xe0, 0x7a, 0x3c),
            strong: Color::Rgb(0x1a, 0x1a, 0x1a),
            selection_bg: Color::Rgb(0xc8, 0xc8, 0xc8),
            selection_fg: Color::Rgb(0x1a, 0x1a, 0x1a),
            lifted: Color::Rgb(0x3a, 0x3a, 0x3a),
            error: Color::Rgb(0x8a, 0x22, 0x18),
        }
    }

    /// The palette for a resolved scheme.
    pub fn for_scheme(scheme: ColorScheme) -> Self {
        match scheme {
            ColorScheme::Dark => Self::dark(),
            ColorScheme::Light => Self::light(),
        }
    }
}

impl Default for Theme {
    /// The Dark palette (`ColorScheme::default()`), phimint's values — what an
    /// unfilled product gets.
    fn default() -> Self {
        Self::dark()
    }
}

/// The class-name half of the layering (design §4.3, Issue 2.2): what a styled
/// run asks for. Semantic variants are value-free names — the run stores the
/// name, never a value, and [`ShellToken::resolve`] reads the value from the
/// stylesheet. [`ShellToken::Literal`] is the one inline-style channel, kept
/// narrow on purpose: brand data only (the product's wordmark generator
/// compiles its gradient ramp into per-letter literals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellToken {
    /// Readable but secondary text — see [`Theme::muted`].
    Muted,
    /// Status-band chrome — see [`Theme::hint`].
    Hint,
    /// Bottom-right status slot — see [`Theme::good`].
    Good,
    /// Quiet chrome — see [`Theme::faint`].
    Faint,
    /// Process text (thinking) — see [`Theme::thought`].
    Thought,
    /// Your own input — see [`Theme::user`].
    User,
    /// The warm badge accent — see [`Theme::warm`].
    Warm,
    /// Must-read foregrounds — see [`Theme::strong`].
    Strong,
    /// Selection highlight background — see [`Theme::selection_bg`].
    SelectionBg,
    /// Selection highlight foreground — see [`Theme::selection_fg`].
    SelectionFg,
    /// Dim-on-highlight foreground — see [`Theme::lifted`].
    Lifted,
    /// The turn-final failure marker — see [`Theme::error`].
    Error,
    /// Inline style: a literal 24-bit color, optionally bold. The brand
    /// channel — compiled wordmark ramps cross the seam this way, and nothing
    /// else may: a product recoloring its stylesheet must not repaint brand.
    Literal { rgb: (u8, u8, u8), bold: bool },
}

impl ShellToken {
    /// Resolve this token against a stylesheet into a paint style. Class
    /// names take their slot's color; `Literal` passes through untouched.
    pub fn resolve(self, theme: Theme) -> Style {
        match self {
            ShellToken::Muted => Style::default().fg(theme.muted),
            ShellToken::Hint => Style::default().fg(theme.hint),
            ShellToken::Good => Style::default().fg(theme.good),
            ShellToken::Faint => Style::default().fg(theme.faint),
            ShellToken::Thought => Style::default().fg(theme.thought),
            ShellToken::User => Style::default().fg(theme.user),
            ShellToken::Warm => Style::default().fg(theme.warm),
            ShellToken::Strong => Style::default().fg(theme.strong),
            ShellToken::SelectionBg => Style::default().fg(theme.selection_bg),
            ShellToken::SelectionFg => Style::default().fg(theme.selection_fg),
            ShellToken::Lifted => Style::default().fg(theme.lifted),
            ShellToken::Error => Style::default().fg(theme.error),
            ShellToken::Literal {
                rgb: (r, g, b),
                bold,
            } => {
                let s = Style::default().fg(Color::Rgb(r, g, b));
                if bold {
                    s.add_modifier(Modifier::BOLD)
                } else {
                    s
                }
            }
        }
    }
}

/// A styled byte-range within an output line's plain text (TUI rendering).
/// The generic span model lives in `phi_tui::lines` (style-token agnostic);
/// this alias pins the shell's token so `SpanSpec { .. }` literals keep
/// working at the shell boundary (`Transcript<ShellToken>` and friends —
/// the generic is pinned there and never spreads).
pub type SpanSpec = phi_tui::lines::SpanSpec<ShellToken>;

/// One styled row crossing the seam (the wordmark channel): plain text plus
/// styled byte-runs, in the same shape `Transcript::push_styled_line` takes.
/// The product's brand layer compiles its role vocabulary into this — literal
/// colors for the ramp, class-name tokens only where it asks for a stylesheet
/// slot — so the shell never learns brand semantics.
#[derive(Debug, Clone)]
pub struct BrandRow {
    /// The row's plain text (copy/selection stays style-blind).
    pub text: String,
    /// Styled byte-runs over `text`, class names or literal colors.
    pub spans: Vec<SpanSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_slot_differs_between_schemes() {
        // A light terminal wearing the dark palette is the failure this module
        // exists to prevent: gray-on-gray, white-on-white chrome.
        let dark = Theme::dark();
        let light = Theme::light();
        for (name, d, l) in [
            ("muted", dark.muted, light.muted),
            ("hint", dark.hint, light.hint),
            ("good", dark.good, light.good),
            ("faint", dark.faint, light.faint),
            ("thought", dark.thought, light.thought),
            ("user", dark.user, light.user),
            ("warm", dark.warm, light.warm),
            ("strong", dark.strong, light.strong),
            ("selection_bg", dark.selection_bg, light.selection_bg),
            ("selection_fg", dark.selection_fg, light.selection_fg),
            ("lifted", dark.lifted, light.lifted),
            ("error", dark.error, light.error),
        ] {
            assert_ne!(d, l, "slot {name} is identical in Dark and Light");
        }
    }

    #[test]
    fn gray_slots_stay_on_the_gray_axis() {
        // Hue is the terminal's job; this module owns grays and the absolute
        // tones only. A stray red here would paint "error" where "hint" is
        // meant.
        for theme in [Theme::dark(), Theme::light()] {
            for (name, c) in [
                ("muted", theme.muted),
                ("hint", theme.hint),
                ("faint", theme.faint),
                ("thought", theme.thought),
                ("lifted", theme.lifted),
            ] {
                match c {
                    Color::Gray | Color::DarkGray => {}
                    Color::Rgb(r, g, b) => {
                        assert_eq!(r, g, "{name} is not gray: {c:?}");
                        assert_eq!(g, b, "{name} is not gray: {c:?}");
                    }
                    other => panic!("{name} left the gray axis: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn lifted_stays_ahead_of_faint_on_the_same_axis() {
        // The selection-lift contract: `lifted` is what a `faint` row becomes
        // on the highlight, so it must not collapse into `faint` itself. On
        // Dark both are palette indices with no RGB of their own, so only the
        // Light pair is orderable here.
        assert_ne!(Theme::light().faint, Theme::light().lifted);
    }

    /// The failure marker inherits the warmth discipline of the palette family
    /// it came from (red channel leading, spread ≥ 20): a subdued warm verdict,
    /// not a neutral gray and not a neon live-error red.
    #[test]
    fn error_stays_warm_on_both_palettes() {
        for (scheme, c) in [
            (ColorScheme::Dark, Theme::dark().error),
            (ColorScheme::Light, Theme::light().error),
        ] {
            let Color::Rgb(r, g, b) = c else {
                panic!("{scheme:?} error is not truecolor: {c:?}");
            };
            assert!(r > g && g >= b, "{scheme:?} not warm-ordered: {r},{g},{b}");
            let spread = i32::from(r.max(g).max(b)) - i32::from(r.min(g).min(b));
            assert!(spread >= 20, "{scheme:?} too neutral: {r},{g},{b}");
        }
    }

    #[test]
    fn class_names_resolve_through_the_stylesheet() {
        // Recolor-by-override: a product that touches one field gets exactly
        // that field repainted.
        let custom = Theme {
            muted: Color::Magenta,
            ..Theme::dark()
        };
        assert_eq!(
            ShellToken::Muted.resolve(custom),
            Style::default().fg(Color::Magenta)
        );
    }

    #[test]
    fn literal_colors_ignore_the_stylesheet() {
        // The brand channel is inline style: a product recoloring every slot
        // must not repaint a compiled wordmark ramp.
        let custom = Theme {
            error: Color::Magenta,
            ..Theme::light()
        };
        assert_eq!(
            ShellToken::Literal {
                rgb: (255, 106, 61),
                bold: true
            }
            .resolve(custom),
            Style::default()
                .fg(Color::Rgb(255, 106, 61))
                .add_modifier(Modifier::BOLD)
        );
    }
}
