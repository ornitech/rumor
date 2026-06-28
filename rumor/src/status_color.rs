//! Status color logic. Kept in its own tiny module so it can be unit-tested
//! without dragging in the rest of the UI / process tree.

use ratatui::style::Color;

/// Color for "in progress" states (waiting on dependencies, starting up).
/// 256-color orange (slot 208). Indexed rather than RGB so it lands on a fixed
/// palette slot instead of being downsampled back toward yellow on non-truecolor
/// terminals, keeping it clearly distinct from both Running-green and the old yellow.
pub const PENDING: Color = Color::Indexed(208);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_is_distinct_from_running() {
        // Guards the "use colors that are more different" intent (#30).
        assert_ne!(PENDING, Color::Green);
        assert_ne!(PENDING, Color::Yellow);
    }
}
