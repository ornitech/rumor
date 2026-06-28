//! Best-effort "copy text to the user's clipboard" for the TUI.
//!
//! Strategy: pipe through the platform clipboard tool when one exists
//! (`pbcopy` on macOS, `wl-copy` / `xclip` on Linux), because those always
//! reach the system clipboard. Fall back to the OSC 52 escape sequence, which
//! works in most modern terminals (iTerm2, kitty, alacritty, wezterm) and
//! over ssh, but not in Terminal.app.

use std::io::Write;
use std::process::{Command, Stdio};

/// Returns true if the text was (probably) copied. OSC 52 success can't be
/// verified — the terminal silently ignores it when unsupported — so a true
/// from the fallback means "sent", not "confirmed".
pub fn copy(text: &str) -> bool {
    for tool in clipboard_tools() {
        if copy_via_tool(tool, text) {
            return true;
        }
    }
    copy_via_osc52(text)
}

#[cfg(target_os = "macos")]
fn clipboard_tools() -> &'static [&'static [&'static str]] {
    &[&["pbcopy"]]
}

#[cfg(not(target_os = "macos"))]
fn clipboard_tools() -> &'static [&'static [&'static str]] {
    &[&["wl-copy"], &["xclip", "-selection", "clipboard"]]
}

fn copy_via_tool(cmd: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(text.as_bytes()).is_ok())
        .unwrap_or(false);
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    wrote && ok
}

fn copy_via_osc52(text: &str) -> bool {
    use crossterm::clipboard::{ClipboardSelection, ClipboardType, CopyToClipboard};
    crossterm::execute!(
        std::io::stdout(),
        CopyToClipboard {
            content: text,
            destination: ClipboardSelection(vec![ClipboardType::Clipboard]),
        }
    )
    .is_ok()
}
