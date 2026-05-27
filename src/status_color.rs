//! Status color logic. Kept in its own tiny module so it can be unit-tested
//! without dragging in the rest of the UI / process tree.

use ratatui::style::Color;

/// Pick a status color for an exited process.
///
/// - Signal-killed (any kind): gray. User/orchestrator stopped it, not an error.
/// - Clean `exit 0`, short-lived: green. One-shot success.
/// - Anything else (non-zero, or long-lived that stopped): red.
pub fn exited_color(code: u32, has_signal: bool, long_lived: bool) -> Color {
    if has_signal {
        Color::DarkGray
    } else if code == 0 && !long_lived {
        Color::Green
    } else {
        Color::Red
    }
}
