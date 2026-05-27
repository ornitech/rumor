//! Truth table for the exit-status color helper.

use ratatui::style::Color;

#[path = "../src/status_color.rs"]
mod status_color;

use status_color::exited_color;

#[test]
fn exit_color_truth_table() {
    // (label, code, has_signal, long_lived, expected)
    let cases: &[(&str, u32, bool, bool, Color)] = &[
        // Signal-killed: always gray, regardless of kind.
        ("sigterm long-lived",  143, true,  true,  Color::DarkGray),
        ("sigterm short-lived", 143, true,  false, Color::DarkGray),
        ("sigint short-lived",  130, true,  false, Color::DarkGray),
        ("sigkill long-lived",  137, true,  true,  Color::DarkGray),
        // Clean exit 0, short-lived: green (one-shot success).
        ("oneshot success", 0, false, false, Color::Green),
        // Clean exit 0, long-lived: red (shouldn't have stopped).
        ("long-lived clean exit", 0, false, true, Color::Red),
        // Non-zero, any kind: red.
        ("oneshot fail 1",   1,   false, false, Color::Red),
        ("oneshot fail 127", 127, false, false, Color::Red),
        ("long-lived fail",  1,   false, true,  Color::Red),
    ];
    for (label, code, has_signal, long_lived, expected) in cases {
        let got = exited_color(*code, *has_signal, *long_lived);
        assert_eq!(
            got, *expected,
            "case '{label}': code={code} has_signal={has_signal} long_lived={long_lived}",
        );
    }
}
