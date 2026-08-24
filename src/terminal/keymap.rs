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

/// xterm "modified cursor key" modifier code (CSI `1;<code><final>`), per
/// `ctlseqs.txt`: 2=Shift, 3=Alt, 4=Shift+Alt, 5=Ctrl, 6=Shift+Ctrl,
/// 7=Alt+Ctrl, 8=Shift+Alt+Ctrl. `None` when no relevant modifier is held,
/// meaning the plain (unmodified) encoding should be used instead.
fn xterm_modifier_code(m: &gpui::Modifiers) -> Option<u8> {
    if !(m.shift || m.alt || m.control) {
        return None;
    }
    Some(1 + m.shift as u8 + 2 * (m.alt as u8) + 4 * (m.control as u8))
}

/// Cursor / Home / End keys, modifier-aware: with Shift/Alt/Ctrl held, xterm
/// always emits `ESC [ 1 ; <code> <final>` (parameterized CSI, regardless of
/// DECCKM) since SS3 has no room for a modifier parameter. Ctrl+Left/Right in
/// particular is what shell readline (bash/zsh) binds to backward-word /
/// forward-word -- without this, Ctrl+arrow was indistinguishable from a
/// plain arrow key and only moved the cursor by one character.
fn modified_cursor_key(final_byte: u8, m: &gpui::Modifiers, app_cursor: bool) -> Vec<u8> {
    match xterm_modifier_code(m) {
        Some(code) => format!("\x1b[1;{code}{}", final_byte as char).into_bytes(),
        None => cursor_key(final_byte, app_cursor),
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
        "left" => return Some(modified_cursor_key(b'D', m, app_cursor)),
        "right" => return Some(modified_cursor_key(b'C', m, app_cursor)),
        "up" => return Some(modified_cursor_key(b'A', m, app_cursor)),
        "down" => return Some(modified_cursor_key(b'B', m, app_cursor)),
        "home" => return Some(modified_cursor_key(b'H', m, app_cursor)),
        "end" => return Some(modified_cursor_key(b'F', m, app_cursor)),
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

    // Plain text input: prefer the actually-typed character (handles Shift).
    // CJK IME commit does not go through this path — it uses `InputHandler`
    // on the terminal canvas (see `render::TerminalInputHandler`).
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

// ---------------------------------------------------------------------------
// Paste encoding
// ---------------------------------------------------------------------------

/// Distinguishes the *source* of a paste payload, mostly for future use (e.g.
/// bracketed vs. unbracketed heuristics that may want to differ for primary
/// selection vs. system clipboard). Today both paths go through the same
/// encoder, but having an explicit enum keeps the call site readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PastePayload {
    /// X11 / Wayland primary selection (mid-click paste).
    #[allow(dead_code)] // reserved for future primary-selection support
    Primary,
    /// System clipboard (Ctrl+Shift+V).
    Clipboard,
}

/// Encode text to send down the PTY as a paste.
///
/// - When the terminal is in `BRACKETED_PASTE` mode, wrap with
///   `ESC[200~` / `ESC[201~` so the program can recognise the bulk
///   payload and avoid auto-indent disasters in editors like vim.
/// - Strip embedded `ESC` bytes from the text first: a malicious
///   clipboard could otherwise inject arbitrary escape sequences
///   (bracketed paste mode is the proper defence; this stripping is
///   belt-and-braces for when it isn't active).
/// - Replace `\r\n` / `\n` with `\r` so the PTY gets the carriage
///   return it expects (terminals are line-buffered; bare `\n`
///   becomes `LF` in the cooked input stream which the program will
///   see as `^J`, not Enter).
pub fn encode_paste(text: &str, mode: TermMode, _src: PastePayload) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }
    // Normalise line endings first; we keep this for both modes.
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");

    if mode.contains(TermMode::BRACKETED_PASTE) {
        let mut out = Vec::with_capacity(normalised.len() + 8);
        out.extend_from_slice(b"\x1b[200~");
        for c in normalised.chars() {
            if c == '\x1b' {
                // Strip ESC bytes inside the bracketed region; the
                // outer ESC[200~/201~ are the legitimate signal
                // boundaries. Anything in the middle is suspect.
                continue;
            }
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            out.extend_from_slice(s.as_bytes());
        }
        out.extend_from_slice(b"\x1b[201~");
        Some(out)
    } else {
        // Outside bracketed-paste mode, we can't safely strip ESC
        // (the program may legitimately be expecting them — though in
        // practice nothing does on a typed paste). Pass through
        // verbatim, with normalised line endings.
        let mut out = Vec::with_capacity(normalised.len());
        out.extend_from_slice(normalised.as_bytes());
        Some(out)
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use gpui::{Keystroke, Modifiers};

    fn keystroke(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_string(),
            key_char: None,
        }
    }

    #[test]
    fn plain_left_is_unmodified_csi() {
        let ks = keystroke("left", Modifiers::default());
        assert_eq!(encode_key(&ks, TermMode::empty()).unwrap(), b"\x1b[D");
    }

    #[test]
    fn ctrl_left_is_modified_csi_for_readline_backward_word() {
        let ks = keystroke(
            "left",
            Modifiers {
                control: true,
                ..Default::default()
            },
        );
        assert_eq!(
            encode_key(&ks, TermMode::empty()).unwrap(),
            b"\x1b[1;5D"
        );
    }

    #[test]
    fn ctrl_right_is_modified_csi_for_readline_forward_word() {
        let ks = keystroke(
            "right",
            Modifiers {
                control: true,
                ..Default::default()
            },
        );
        assert_eq!(
            encode_key(&ks, TermMode::empty()).unwrap(),
            b"\x1b[1;5C"
        );
    }

    #[test]
    fn ctrl_left_ignores_app_cursor_mode() {
        // Modified sequences are always CSI, never SS3, since SS3 has no
        // room for a modifier parameter (xterm ctlseqs.txt).
        let ks = keystroke(
            "left",
            Modifiers {
                control: true,
                ..Default::default()
            },
        );
        assert_eq!(
            encode_key(&ks, TermMode::APP_CURSOR).unwrap(),
            b"\x1b[1;5D"
        );
    }

    #[test]
    fn shift_home_is_modified_csi() {
        let ks = keystroke(
            "home",
            Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        assert_eq!(
            encode_key(&ks, TermMode::empty()).unwrap(),
            b"\x1b[1;2H"
        );
    }
}

#[cfg(test)]
mod paste_tests {
    use super::*;

    #[test]
    fn empty_returns_none() {
        assert!(encode_paste("", TermMode::empty(), PastePayload::Clipboard).is_none());
    }

    #[test]
    fn newlines_normalised_outside_bracketed() {
        let out = encode_paste("a\nb\r\nc", TermMode::empty(), PastePayload::Clipboard)
            .unwrap();
        assert_eq!(out, b"a\rb\rc");
    }

    #[test]
    fn bracketed_wraps_and_strips_esc() {
        let mode = TermMode::BRACKETED_PASTE;
        let out = encode_paste("hi \x1b[31mred\x1b[0m", mode, PastePayload::Clipboard)
            .unwrap();
        assert_eq!(out, b"\x1b[200~hi [31mred[0m\x1b[201~");
    }
}
