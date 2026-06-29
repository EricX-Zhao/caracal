//! GPUI keystroke -> terminal byte sequence encoding.
//!
//! Phase 1 covers the common cases; Phase 2 hardens this (the DECCKM cursor-key
//! handling is already wired here since it reads `TermMode` rather than guessing).
//! No local echo: the remote/PTY echoes input back.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// Cursor / Home / End keys: SS3 (`ESC O x`) when the terminal is in application
/// cursor mode (DECCKM), otherwise CSI (`ESC [ x`).
fn cursor_key(final_byte: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// Control byte for a Ctrl-<char> combination, per the classic ASCII mapping.
fn ctrl_byte(c: char) -> Option<u8> {
    let b = c as u32;
    match c {
        'a'..='z' => Some((c as u8 - b'a') + 1),
        'A'..='Z' => Some((c as u8 - b'A') + 1),
        '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        ' ' => Some(0x00),
        _ if b < 0x80 => None,
        _ => None,
    }
}

/// Encode a keystroke into the bytes to send to the backend. Returns `None` for
/// keys that produce no output (bare modifiers, unhandled keys).
pub fn encode_key(ks: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let key = ks.key.as_str();

    // Named / special keys take precedence over text.
    match key {
        "enter" => return Some(b"\r".to_vec()),
        "tab" => {
            return Some(if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            });
        }
        "backspace" => {
            return Some(if m.alt {
                vec![0x1b, 0x7f]
            } else {
                vec![0x7f]
            });
        }
        "escape" => return Some(vec![0x1b]),
        "space" if m.control => return Some(vec![0x00]),
        "left" => return Some(cursor_key(b'D', app_cursor)),
        "right" => return Some(cursor_key(b'C', app_cursor)),
        "up" => return Some(cursor_key(b'A', app_cursor)),
        "down" => return Some(cursor_key(b'B', app_cursor)),
        "home" => return Some(cursor_key(b'H', app_cursor)),
        "end" => return Some(cursor_key(b'F', app_cursor)),
        "pageup" => return Some(b"\x1b[5~".to_vec()),
        "pagedown" => return Some(b"\x1b[6~".to_vec()),
        "delete" => return Some(b"\x1b[3~".to_vec()),
        "insert" => return Some(b"\x1b[2~".to_vec()),
        "f1" => return Some(b"\x1bOP".to_vec()),
        "f2" => return Some(b"\x1bOQ".to_vec()),
        "f3" => return Some(b"\x1bOR".to_vec()),
        "f4" => return Some(b"\x1bOS".to_vec()),
        "f5" => return Some(b"\x1b[15~".to_vec()),
        "f6" => return Some(b"\x1b[17~".to_vec()),
        "f7" => return Some(b"\x1b[18~".to_vec()),
        "f8" => return Some(b"\x1b[19~".to_vec()),
        "f9" => return Some(b"\x1b[20~".to_vec()),
        "f10" => return Some(b"\x1b[21~".to_vec()),
        "f11" => return Some(b"\x1b[23~".to_vec()),
        "f12" => return Some(b"\x1b[24~".to_vec()),
        _ => {}
    }

    // Ctrl-<char> -> control byte (Alt adds an ESC prefix).
    if m.control {
        let mut chars = key.chars();
        if let (Some(c), None) = (chars.next(), chars.clone().next()) {
            if let Some(b) = ctrl_byte(c) {
                let mut out = Vec::with_capacity(2);
                if m.alt {
                    out.push(0x1b);
                }
                out.push(b);
                return Some(out);
            }
        }
        return None;
    }

    // Plain text input: prefer the actually-typed character (handles shift/IME).
    let text = ks
        .key_char
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| match key {
            "space" => Some(" ".to_string()),
            k if k.chars().count() == 1 => Some(k.to_string()),
            _ => None,
        })?;

    let mut out = Vec::with_capacity(text.len() + 1);
    if m.alt {
        out.push(0x1b); // Alt-<x> -> ESC prefix
    }
    out.extend_from_slice(text.as_bytes());
    Some(out)
}
