//! Keyboard input to terminal byte sequences.

use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn modifier_param(mods: ModifiersState) -> u8 {
    1 + mods.shift_key() as u8 + (mods.alt_key() as u8) * 2 + (mods.control_key() as u8) * 4
}

// Keys that use 'CSI 1;m X' with a modifier and 'SS3 X' / 'CSI X' without.
// CSI: Control Sequence Introducer
// SS3: Single Shift 3
fn cursor_key(c: u8, mods: ModifiersState, app: bool) -> Vec<u8> {
    let m = modifier_param(mods);
    if m > 1 {
        // \x1b: escape
        format!("\x1b[1;{m}{}", c as char).into_bytes()
    } else if app {
        vec![0x1b, b'O', c]
    } else {
        vec![0x1b, b'[', c]
    }
}

fn tilde_key(n: u8, mods: ModifiersState) -> Vec<u8> {
    let m = modifier_param(mods);
    if m > 1 {
        format!("\x1b[{n};{m}~").into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}

// Encodes a key press. 'mode' matters because an application that set
// DECCKM expects SS3 arrows; without this the arrows stop working in
// vim and emacs.
pub fn encode(key: &Key, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    let app = mode.contains(TermMode::APP_CURSOR);
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let shift = mods.shift_key();

    let mut bytes = match key {
        Key::Named(NamedKey::Enter) => b"\r".to_vec(),
        Key::Named(NamedKey::Tab) if shift => b"\x1b[Z".to_vec(),
        Key::Named(NamedKey::Tab) => b"\t".to_vec(),
        Key::Named(NamedKey::Backspace) => vec![0x7f],
        Key::Named(NamedKey::Escape) => vec![0x1b],
        Key::Named(NamedKey::Space) if ctrl => vec![0x00],
        Key::Named(NamedKey::Space) => b" ".to_vec(),
        Key::Named(NamedKey::ArrowUp) => cursor_key(b'A', mods, app),
        Key::Named(NamedKey::ArrowDown) => cursor_key(b'B', mods, app),
        Key::Named(NamedKey::ArrowRight) => cursor_key(b'C', mods, app),
        Key::Named(NamedKey::ArrowLeft) => cursor_key(b'D', mods, app),
        Key::Named(NamedKey::Home) => cursor_key(b'H', mods, app),
        Key::Named(NamedKey::End) => cursor_key(b'F', mods, app),
        Key::Named(NamedKey::F1) => cursor_key(b'P', mods, true),
        Key::Named(NamedKey::F2) => cursor_key(b'Q', mods, true),
        Key::Named(NamedKey::F3) => cursor_key(b'R', mods, true),
        Key::Named(NamedKey::F4) => cursor_key(b'S', mods, true),
        Key::Named(NamedKey::Insert) => tilde_key(2, mods),
        Key::Named(NamedKey::Delete) => tilde_key(3, mods),
        Key::Named(NamedKey::PageUp) => tilde_key(5, mods),
        Key::Named(NamedKey::PageDown) => tilde_key(6, mods),
        Key::Named(NamedKey::F5) => tilde_key(15, mods),
        Key::Named(NamedKey::F6) => tilde_key(17, mods),
        Key::Named(NamedKey::F7) => tilde_key(18, mods),
        Key::Named(NamedKey::F8) => tilde_key(19, mods),
        Key::Named(NamedKey::F9) => tilde_key(20, mods),
        Key::Named(NamedKey::F10) => tilde_key(21, mods),
        Key::Named(NamedKey::F11) => tilde_key(23, mods),
        Key::Named(NamedKey::F12) => tilde_key(24, mods),
        Key::Character(s) if ctrl => vec![control_byte(s.chars().next()?)?],
        Key::Character(s) => s.as_bytes().to_vec(),
        _ => return None,
    };

    // Alt is an ESC prefix for everything except sequences that already
    // encode the modifier (those start with ESC themselves).
    if alt && !bytes.starts_with(b"\x1b") {
        bytes.insert(0, 0x1b);
    }

    Some(bytes)
}

fn control_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    match c {
        'a'..='z' => Some(c as u8 & 0x1f),
        '@' | ' ' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
        encode(&key, mods, mode)
    }
    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }
    const NONE: ModifiersState = ModifiersState::empty();

    #[test]
    fn enter_sends_cr_not_lf() {
        assert_eq!(
            enc(Key::Named(NamedKey::Enter), NONE, TermMode::NONE).unwrap(),
            b"\r"
        );
    }

    #[test]
    fn plain_character_passes_through() {
        assert_eq!(enc(ch("a"), NONE, TermMode::NONE).unwrap(), b"a");
        assert_eq!(
            enc(ch("あ"), NONE, TermMode::NONE).unwrap(),
            "あ".as_bytes()
        );
    }

    #[test]
    fn ctrl_letter_is_control_byte() {
        assert_eq!(
            enc(ch("c"), ModifiersState::CONTROL, TermMode::NONE).unwrap(),
            [0x03]
        );
        assert_eq!(
            enc(
                ch("C"),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                TermMode::NONE
            )
            .unwrap(),
            [0x03]
        );
        assert_eq!(
            enc(ch("["), ModifiersState::CONTROL, TermMode::NONE).unwrap(),
            [0x1b]
        );
        assert_eq!(
            enc(
                Key::Named(NamedKey::Space),
                ModifiersState::CONTROL,
                TermMode::NONE
            )
            .unwrap(),
            [0x00]
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            enc(ch("x"), ModifiersState::ALT, TermMode::NONE).unwrap(),
            b"\x1bx"
        );
    }

    #[test]
    fn arrows_follow_app_cursor_mode() {
        assert_eq!(
            enc(Key::Named(NamedKey::ArrowUp), NONE, TermMode::NONE).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            enc(Key::Named(NamedKey::ArrowUp), NONE, TermMode::APP_CURSOR).unwrap(),
            b"\x1bOA"
        );
    }

    #[test]
    fn modified_arrow_uses_csi_with_modifier_even_in_app_mode() {
        assert_eq!(
            enc(
                Key::Named(NamedKey::ArrowRight),
                ModifiersState::CONTROL,
                TermMode::APP_CURSOR
            )
            .unwrap(),
            b"\x1b[1;5C"
        );
    }

    #[test]
    fn shift_tab_is_backtab() {
        assert_eq!(
            enc(
                Key::Named(NamedKey::Tab),
                ModifiersState::SHIFT,
                TermMode::NONE
            )
            .unwrap(),
            b"\x1b[Z"
        );
    }

    #[test]
    fn tilde_keys_carry_modifier() {
        assert_eq!(
            enc(Key::Named(NamedKey::Delete), NONE, TermMode::NONE).unwrap(),
            b"\x1b[3~"
        );
        assert_eq!(
            enc(
                Key::Named(NamedKey::PageUp),
                ModifiersState::SHIFT,
                TermMode::NONE
            )
            .unwrap(),
            b"\x1b[5;2~"
        );
        assert_eq!(
            enc(Key::Named(NamedKey::F5), NONE, TermMode::NONE).unwrap(),
            b"\x1b[15~"
        );
        assert_eq!(
            enc(Key::Named(NamedKey::F1), NONE, TermMode::NONE).unwrap(),
            b"\x1bOP"
        );
    }

    #[test]
    fn backspace_is_del_byte() {
        assert_eq!(
            enc(Key::Named(NamedKey::Backspace), NONE, TermMode::NONE).unwrap(),
            [0x7f]
        );
        assert_eq!(
            enc(
                Key::Named(NamedKey::Backspace),
                ModifiersState::ALT,
                TermMode::NONE
            )
            .unwrap(),
            [0x1b, 0x7f]
        );
    }

    #[test]
    fn unhandled_key_is_none() {
        assert!(
            enc(
                Key::Named(NamedKey::Shift),
                ModifiersState::SHIFT,
                TermMode::NONE
            )
            .is_none()
        );
    }
}
