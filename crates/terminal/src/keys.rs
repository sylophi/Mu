//! Keystrokes to the bytes a terminal sends for them.
//!
//! Plain printable text is left to the input handler so IME and dead keys work. This covers
//! named keys and modifier combinations, as an xterm sends them, or in the kitty keyboard
//! protocol to programs that ask for it (Claude Code, Codex, Neovim), which tells apart keys an
//! xterm sends the same, like Shift+Enter and Enter.

use alacritty_terminal::term::TermMode;
use gpui_kit::Keystroke;

pub fn to_esc(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    if let Some(edit) = mac_edit(keystroke) {
        return Some(edit.to_vec());
    }
    // Other Cmd shortcuts belong to the app.
    if m.platform {
        return None;
    }

    // xterm's modifier parameter, and kitty's: 1, plus 1 for shift, 2 for alt and 4 for ctrl.
    let modifier = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.control as u8;
    let kitty = mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL);
    if kitty && let Some(number) = kitty_number(keystroke) {
        return Some(csi(number, modifier, 'u'));
    }
    // Kitty sends these without SS3.
    let app_cursor = mode.contains(TermMode::APP_CURSOR) && !kitty;

    // Arrows, Home and End, and F1-F4 (`ss3`): `ESC [ 1 ; m A` with modifiers, else `ESC O A` or `ESC [ A`.
    let letter = |letter: char, ss3: bool| -> Vec<u8> {
        if ss3 && modifier == 1 { format!("\x1bO{letter}").into_bytes() } else { csi(1, modifier, letter) }
    };
    let tilde = |code: u32| csi(code, modifier, '~');
    let meta = |bytes: &[u8]| -> Vec<u8> { if m.alt { [b"\x1b".as_slice(), bytes].concat() } else { bytes.to_vec() } };

    let bytes = match keystroke.key.as_str() {
        "up" => letter('A', app_cursor),
        "down" => letter('B', app_cursor),
        "right" => letter('C', app_cursor),
        "left" => letter('D', app_cursor),
        "home" => letter('H', app_cursor),
        "end" => letter('F', app_cursor),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => letter('P', !kitty),
        "f2" => letter('Q', !kitty),
        // Kitty's, as `CSI R` is also a cursor position report.
        "f3" if kitty => tilde(13),
        "f3" => letter('R', true),
        "f4" => letter('S', !kitty),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "enter" => meta(b"\r"),
        "escape" => meta(b"\x1b"),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => meta(b"\t"),
        "backspace" if m.control => meta(b"\x08"),
        "backspace" => meta(b"\x7f"),
        "space" if m.control => meta(b"\x00"),
        "space" if m.alt => b"\x1b ".to_vec(),
        key if m.control => meta(&[control_byte(key)?]),
        _ if option_types(keystroke) => return None,
        // Option as meta: ESC followed by the unmodified key.
        key if m.alt && key.chars().count() == 1 => {
            let key = if m.shift { key.to_uppercase() } else { key.to_string() };
            meta(key.as_bytes())
        }
        _ => return None,
    };
    Some(bytes)
}

/// A Mac's Cmd and Option edits, as the line editing keys every shell and TUI reads: to the
/// line's start and end, deleting to its start or end, a word back and forward, and deleting the
/// word ahead. They're the same in the kitty protocol, as in other terminals.
fn mac_edit(keystroke: &Keystroke) -> Option<&'static [u8]> {
    let m = &keystroke.modifiers;
    if m.control || m.shift {
        return None;
    }
    Some(match (m.platform, m.alt, keystroke.key.as_str()) {
        (true, false, "left") => b"\x01",
        (true, false, "right") => b"\x05",
        (true, false, "backspace") => b"\x15",
        (true, false, "delete") => b"\x0b",
        (false, true, "left") => b"\x1bb",
        (false, true, "right") => b"\x1bf",
        (false, true, "delete") => b"\x1bd",
        _ => return None,
    })
}

/// Option types what the layout puts on it (@ on German ⌥L), like Terminal by default.
fn option_types(keystroke: &Keystroke) -> bool {
    let m = &keystroke.modifiers;
    m.alt && !m.control && keystroke.key_char.as_ref().is_some_and(|typed| *typed != keystroke.key)
}

/// The number the kitty protocol sends a key as, `CSI number ; modifier u`, unless it sends the
/// key as an xterm does. Keys that type, alone or with Shift, still type, and Enter, Tab and
/// Backspace alone are as they were, so a shell still works after a program that turned the
/// protocol on quits without turning it off.
fn kitty_number(keystroke: &Keystroke) -> Option<u32> {
    let m = &keystroke.modifiers;
    let alone = !m.shift && !m.alt && !m.control;
    let typing = !m.alt && !m.control;
    let (number, as_before) = match keystroke.key.as_str() {
        "escape" => (27, false),
        "enter" => (13, alone),
        "tab" => (9, alone),
        "backspace" => (127, alone),
        "space" => (32, typing),
        key => {
            let mut chars = key.chars();
            let (Some(char), None) = (chars.next(), chars.next()) else {
                return None;
            };
            (char.into(), typing || option_types(keystroke))
        }
    };
    (!as_before).then_some(number)
}

/// `CSI number ; modifier end`, without the modifier when there's none, and then without a number
/// of 1.
fn csi(number: u32, modifier: u8, end: char) -> Vec<u8> {
    match (number, modifier) {
        (_, 2..) => format!("\x1b[{number};{modifier}{end}"),
        (1, _) => format!("\x1b[{end}"),
        _ => format!("\x1b[{number}{end}"),
    }
    .into_bytes()
}

fn control_byte(key: &str) -> Option<u8> {
    let &[byte] = key.as_bytes() else {
        return None;
    };
    Some(match byte {
        b'a'..=b'z' => byte - b'a' + 1,
        b'@' | b'2' => 0x00,
        b'[' | b'3' => 0x1b,
        b'\\' | b'4' => 0x1c,
        b']' | b'5' => 0x1d,
        b'^' | b'6' => 0x1e,
        b'_' | b'-' | b'/' | b'7' => 0x1f,
        b'8' | b'?' => 0x7f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(keystroke: &str, mode: TermMode) -> Option<Vec<u8>> {
        to_esc(&Keystroke::parse(keystroke).unwrap(), mode)
    }

    #[test]
    fn named_keys_and_modifiers() {
        let normal = TermMode::empty();
        assert_eq!(esc("ctrl-c", normal), Some(vec![3]));
        assert_eq!(esc("ctrl-space", normal), Some(vec![0]));
        assert_eq!(esc("up", normal), Some(b"\x1b[A".to_vec()));
        assert_eq!(esc("up", TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        assert_eq!(esc("shift-up", TermMode::APP_CURSOR), Some(b"\x1b[1;2A".to_vec()));
        assert_eq!(esc("ctrl-alt-delete", normal), Some(b"\x1b[3;7~".to_vec()));
        assert_eq!(esc("shift-tab", normal), Some(b"\x1b[Z".to_vec()));
        assert_eq!(esc("alt-b", normal), Some(b"\x1bb".to_vec()));
        assert_eq!(esc("alt-shift-b", normal), Some(b"\x1bB".to_vec()));
        assert_eq!(esc("alt-enter", normal), Some(b"\x1b\r".to_vec()));
        assert_eq!(esc("f1", normal), Some(b"\x1bOP".to_vec()));
        // Cmd shortcuts are the app's, and plain text goes through the input handler.
        assert_eq!(esc("cmd-c", normal), None);
        assert_eq!(esc("a", normal), None);
    }

    #[test]
    fn mac_edits() {
        for mode in [TermMode::empty(), TermMode::DISAMBIGUATE_ESC_CODES] {
            assert_eq!(esc("cmd-backspace", mode), Some(b"\x15".to_vec()));
            assert_eq!(esc("cmd-left", mode), Some(b"\x01".to_vec()));
            assert_eq!(esc("cmd-right", mode), Some(b"\x05".to_vec()));
            assert_eq!(esc("cmd-delete", mode), Some(b"\x0b".to_vec()));
            assert_eq!(esc("alt-left", mode), Some(b"\x1bb".to_vec()));
            assert_eq!(esc("alt-right", mode), Some(b"\x1bf".to_vec()));
            assert_eq!(esc("alt-delete", mode), Some(b"\x1bd".to_vec()));
            assert_eq!(esc("cmd-shift-left", mode), None);
        }
    }

    #[test]
    fn kitty() {
        let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(esc("shift-enter", kitty), Some(b"\x1b[13;2u".to_vec()));
        assert_eq!(esc("ctrl-c", kitty), Some(b"\x1b[99;5u".to_vec()));
        assert_eq!(esc("ctrl-shift-c", kitty), Some(b"\x1b[99;6u".to_vec()));
        assert_eq!(esc("alt-b", kitty), Some(b"\x1b[98;3u".to_vec()));
        assert_eq!(esc("escape", kitty), Some(b"\x1b[27u".to_vec()));
        assert_eq!(esc("shift-tab", kitty), Some(b"\x1b[9;2u".to_vec()));
        assert_eq!(esc("alt-backspace", kitty), Some(b"\x1b[127;3u".to_vec()));
        assert_eq!(esc("ctrl-space", kitty), Some(b"\x1b[32;5u".to_vec()));
        assert_eq!(esc("up", kitty | TermMode::APP_CURSOR), Some(b"\x1b[A".to_vec()));
        assert_eq!(esc("f3", kitty), Some(b"\x1b[13~".to_vec()));
        // As before, so a shell still works if a program leaves the protocol on.
        assert_eq!(esc("enter", kitty), Some(b"\r".to_vec()));
        assert_eq!(esc("tab", kitty), Some(b"\t".to_vec()));
        assert_eq!(esc("backspace", kitty), Some(b"\x7f".to_vec()));
        // Typing.
        assert_eq!(esc("a", kitty), None);
        assert_eq!(esc("shift-a", kitty), None);
        assert_eq!(esc("space", kitty), None);
    }

    #[test]
    fn option_types_what_the_layout_puts_on_it() {
        let mut keystroke = Keystroke::parse("alt-l").unwrap();
        keystroke.key_char = Some("@".into());
        assert_eq!(to_esc(&keystroke, TermMode::empty()), None);
        assert_eq!(to_esc(&keystroke, TermMode::DISAMBIGUATE_ESC_CODES), None);
    }
}
