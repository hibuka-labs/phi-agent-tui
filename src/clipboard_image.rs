//! Clipboard → image bytes, for the paste pipeline.
//!
//! One exit-code contract, three platform defaults, and an env seam so tests
//! and PTY runs inject their own grabber:
//!
//! - `PHIMINT_CLIPBOARD_IMAGE_HELPER` — a command string run via `sh -c`
//!   (unix) / `cmd /C` (windows) instead of the platform default.
//!
//! Helper contract (also what the platform defaults emit):
//!
//! - exit 0 + stdout bytes  → the image
//! - exit 0 + empty stdout  → no image on the clipboard (lenient)
//! - exit 3                 → no image on the clipboard (explicit)
//! - any other exit         → an error; stderr is the message shown to the user

use std::process::{Command, Stdio};

/// Env seam: when set, this command replaces the platform clipboard grabber.
pub const CLIPBOARD_IMAGE_HELPER_ENV: &str = "PHIMINT_CLIPBOARD_IMAGE_HELPER";

/// Grab the clipboard's image, honoring the `PHIMINT_CLIPBOARD_IMAGE_HELPER`
/// seam when it is set.
pub fn grab_clipboard_image() -> Result<Option<Vec<u8>>, String> {
    match std::env::var(CLIPBOARD_IMAGE_HELPER_ENV) {
        Ok(helper) if !helper.trim().is_empty() => grab_clipboard_image_with(Some(&helper)),
        _ => grab_clipboard_image_with(None),
    }
}

/// Grab the clipboard's image: `Some(helper)` runs that command under the
/// exit-code contract, `None` uses the platform default. `Ok(None)` means
/// there is no image on the clipboard — the caller falls back to text paste.
pub fn grab_clipboard_image_with(helper: Option<&str>) -> Result<Option<Vec<u8>>, String> {
    match helper {
        Some(helper) => run_helper(helper),
        None => run_platform_default(),
    }
}

/// Run a helper command through the platform shell and interpret its
/// exit code / stdout / stderr under the contract above.
fn run_helper(helper: &str) -> Result<Option<Vec<u8>>, String> {
    #[cfg(windows)]
    let output = Command::new("cmd")
        .args(["/C", helper])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    #[cfg(not(windows))]
    let output = Command::new("sh")
        .args(["-c", helper])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    let output = output.map_err(|e| format!("cannot run the clipboard helper: {e}"))?;
    interpret(output.status.code(), output.stdout, output.stderr)
}

/// The exit-code contract, in one place.
fn interpret(
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Result<Option<Vec<u8>>, String> {
    match code {
        Some(0) if stdout.is_empty() => Ok(None),
        Some(0) => Ok(Some(stdout)),
        Some(3) => Ok(None),
        _ => {
            let msg = String::from_utf8_lossy(&stderr).trim().to_string();
            if msg.is_empty() {
                Err(match code {
                    Some(c) => format!("clipboard helper failed with exit code {c}"),
                    None => "clipboard helper died on a signal".to_string(),
                })
            } else {
                Err(msg)
            }
        }
    }
}

// ── Platform defaults ──

#[cfg(target_os = "macos")]
fn run_platform_default() -> Result<Option<Vec<u8>>, String> {
    // AppleScript cannot write binary to stdout, so it drops the clipboard's
    // PNG flavor («class PNGf») into a temp file and we read it back.
    let tmp = std::env::temp_dir().join(format!(
        "phimint-clip-{}-{:09}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    const SCRIPT: &str = "on run argv\n\
        \tset outPath to item 1 of argv\n\
        \tset fd to open for access POSIX file outPath with write permission\n\
        \ttry\n\
        \t\twrite (the clipboard as «class PNGf») to fd\n\
        \t\tclose access fd\n\
        \ton error errMsg number errNum\n\
        \t\ttry\n\
        \t\t\tclose access fd\n\
        \t\tend try\n\
        \t\terror errMsg number errNum\n\
        \tend try\n\
        \tend run";
    let output = Command::new("osascript")
        .arg("-e")
        .arg(SCRIPT)
        .arg("--")
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("osascript is unavailable: {e}"))?;
    let bytes = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    // A failure is the common case: the clipboard holds no PNG. Report it as
    // "no image" so the caller falls back to text paste.
    if !output.status.success() {
        return Ok(None);
    }
    if bytes.is_empty() {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

#[cfg(target_os = "windows")]
fn run_platform_default() -> Result<Option<Vec<u8>>, String> {
    const SCRIPT: &str = "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
        $img = [System.Windows.Forms.Clipboard]::GetImage(); \
        if ($null -eq $img) { exit 3 }; \
        $img.Save([System.Console]::OpenStandardOutput(), \
        [System.Drawing.Imaging.ImageFormat]::Png)";
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("powershell is unavailable: {e}"))?;
    interpret(output.status.code(), output.stdout, output.stderr)
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn run_platform_default() -> Result<Option<Vec<u8>>, String> {
    // Wayland first, then X11. A tool that runs but finds no image exits 3;
    // no tool at all is an error the user can act on.
    const SCRIPT: &str = "if command -v wl-paste >/dev/null 2>&1; then \
            wl-paste --type image/png 2>/dev/null; rc=$?; \
            [ $rc -eq 0 ] || exit 3; exit 0; \
        fi; \
        if command -v xclip >/dev/null 2>&1; then \
            xclip -selection clipboard -t image/png -o 2>/dev/null; rc=$?; \
            [ $rc -eq 0 ] || exit 3; exit 0; \
        fi; \
        echo 'no clipboard image tool found (install wl-paste or xclip)' >&2; \
        exit 1";
    run_helper(SCRIPT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// protects: bytes come back byte-identical, and the seam is what ran.
    /// fails_when: the grabber re-encodes or the seam is ignored. no_claim:
    /// what real clipboards hold.
    #[cfg(unix)]
    #[test]
    fn helper_bytes_come_back_untouched() {
        let got = grab_clipboard_image_with(Some("printf 'fakepng'")).expect("helper answer");
        assert_eq!(got, Some(b"fakepng".to_vec()));
    }

    /// protects: exit 3 is the explicit "no image" answer, even when the
    /// helper wrote stdout before deciding. fails_when: exit 3 is read as an
    /// error or as data. no_claim: which helpers use which code.
    #[cfg(unix)]
    #[test]
    fn exit_three_means_no_image() {
        assert_eq!(
            grab_clipboard_image_with(Some("exit 3")).expect("answer"),
            None
        );
        assert_eq!(
            grab_clipboard_image_with(Some("printf x; exit 3")).expect("answer"),
            None
        );
    }

    /// protects: empty success is no image (lenient contract). fails_when:
    /// an empty stdout becomes an empty-image error. no_claim: whether real
    /// helpers ever emit zero-byte images.
    #[cfg(unix)]
    #[test]
    fn empty_success_is_no_image() {
        assert_eq!(
            grab_clipboard_image_with(Some("true")).expect("answer"),
            None
        );
    }

    /// protects: a failing helper surfaces its stderr as the error message
    /// (AC6: errors are shown, never swallowed). fails_when: the failure is
    /// reported as "no image" or as a bare code. no_claim: wording of the
    /// helper's own message.
    #[cfg(unix)]
    #[test]
    fn failure_reports_the_stderr() {
        let err = grab_clipboard_image_with(Some("echo oops >&2; exit 1")).expect_err("must fail");
        assert_eq!(err, "oops");
    }

    /// protects: a failure with silent stderr still names a cause — the user
    /// sees something actionable, not nothing. fails_when: the error message
    /// is empty. no_claim: the exact fallback wording.
    #[cfg(unix)]
    #[test]
    fn silent_failure_still_names_a_cause() {
        let err = grab_clipboard_image_with(Some("exit 7")).expect_err("must fail");
        assert!(err.contains("7"), "got: {err}");
    }

    /// protects: a helper that does not exist is an error, not "no image".
    /// fails_when: a broken install silently drops to text paste with no
    /// word. no_claim: the shell's exact not-found wording.
    #[cfg(unix)]
    #[test]
    fn a_missing_helper_command_is_an_error() {
        let err = grab_clipboard_image_with(Some("definitely-not-a-command-phimint"))
            .expect_err("must fail");
        assert!(!err.is_empty(), "the failure names a cause");
    }
}
