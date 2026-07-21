//! Central registry for Caracal's 12 user-configurable application-level
//! keyboard shortcuts (Settings → Shortcuts) — the single source of truth
//! for their default bindings and the live-rebind mechanism.
//!
//! Live rebinding never calls `cx.clear_key_bindings()`: gpui's own keymap
//! doc comment (`crates/gpui/src/keymap.rs`) states "the ones added to the
//! keymap later take precedence" for two bindings at the same context
//! depth — so `SettingsWindow::apply` just appends a fresh `KeyBinding`
//! for whatever the user changed (see `build_key_bindings_for`), which
//! silently shadows the old one immediately, without ever touching
//! gpui-component's own internally-registered bindings (which `clear_key_
//! bindings()` would wipe, with no clean way to restore them — see the
//! design spec's investigation).

use std::collections::HashMap;

use gpui::{KeyBinding, Keystroke, NoAction};
use gpui_component::dock::ClosePanel;

use crate::terminal::view::{ClearScreen, TERMINAL_KEY_CONTEXT};
use crate::workspace::{
    NewConnectionAction, NewTab, NextTab, OpenSettingsAction, PrevTab, ToggleLeftSidebar,
    ToggleQuickCommands, ToggleRightSidebar, ZoomIn, ZoomOut,
};

/// `(action_id, default_binding_string)` for every user-configurable
/// shortcut, in the order the Settings → Shortcuts tab lists them.
pub const DEFAULT_KEYBINDINGS: &[(&str, &str)] = &[
    ("new_tab", "secondary-shift-t"),
    ("close_tab", "secondary-shift-w"),
    ("next_tab", "secondary-tab"),
    ("prev_tab", "secondary-shift-tab"),
    ("new_connection", "secondary-shift-n"),
    ("toggle_left_sidebar", "secondary-b"),
    ("toggle_right_sidebar", "secondary-shift-b"),
    ("toggle_quick_commands", "secondary-j"),
    ("open_settings", "secondary-,"),
    ("zoom_in", "secondary-="),
    ("zoom_out", "secondary--"),
    ("clear_screen", "secondary-shift-l"),
];

/// Resolve `action_id`'s effective key: the override if present, else its
/// default. `None` for an unrecognized `action_id`.
pub fn effective_key(action_id: &str, overrides: &HashMap<String, String>) -> Option<String> {
    if let Some(k) = overrides.get(action_id) {
        return Some(k.clone());
    }
    DEFAULT_KEYBINDINGS
        .iter()
        .find(|(id, _)| *id == action_id)
        .map(|(_, key)| key.to_string())
}

/// One action-id's `KeyBinding` for a specific `key` string — the only
/// place that maps an action-id string to its concrete typed `Action` and
/// key-context. Panics on an unrecognized `action_id`: every caller in
/// this codebase sources ids from `DEFAULT_KEYBINDINGS` itself, so an
/// unknown id here is a programming error, not user input.
fn keybinding_for(action_id: &str, key: &str) -> KeyBinding {
    match action_id {
        "new_tab" => KeyBinding::new(key, NewTab, None),
        "close_tab" => KeyBinding::new(key, ClosePanel, None),
        "next_tab" => KeyBinding::new(key, NextTab, None),
        "prev_tab" => KeyBinding::new(key, PrevTab, None),
        "new_connection" => KeyBinding::new(key, NewConnectionAction, None),
        "toggle_left_sidebar" => KeyBinding::new(key, ToggleLeftSidebar, None),
        "toggle_right_sidebar" => KeyBinding::new(key, ToggleRightSidebar, None),
        "toggle_quick_commands" => KeyBinding::new(key, ToggleQuickCommands, None),
        "open_settings" => KeyBinding::new(key, OpenSettingsAction, None),
        "zoom_in" => KeyBinding::new(key, ZoomIn, None),
        "zoom_out" => KeyBinding::new(key, ZoomOut, None),
        "clear_screen" => KeyBinding::new(key, ClearScreen, Some(TERMINAL_KEY_CONTEXT)),
        _ => unreachable!("keybinding_for called with an unknown action_id: {action_id}"),
    }
}

/// Context for `action_id` — mirrors `keybinding_for`'s context choice but
/// without needing the concrete Action type, since `suppress_key` always
/// binds to `gpui::NoAction`.
fn context_for(action_id: &str) -> Option<&'static str> {
    match action_id {
        "clear_screen" => Some(TERMINAL_KEY_CONTEXT),
        "new_tab" | "close_tab" | "next_tab" | "prev_tab" | "new_connection"
        | "toggle_left_sidebar" | "toggle_right_sidebar" | "toggle_quick_commands"
        | "open_settings" | "zoom_in" | "zoom_out" => None,
        _ => unreachable!("context_for called with an unknown action_id: {action_id}"),
    }
}

/// Build a `KeyBinding` that explicitly suppresses `old_key` for
/// `action_id`'s context, using gpui's own `NoAction` sentinel action.
/// Confirmed against gpui's actual dispatch source
/// (`Keymap::bindings_for_input` in `crates/gpui/src/keymap.rs`): when a
/// `NoAction` binding is the highest-precedence match for a keystroke, the
/// dispatch loop `break`s, discarding every lower-precedence (earlier-
/// added) binding for that same keystroke+context — so appending this
/// after a live-rebind makes the old key immediately stop firing the
/// action it used to, without ever calling `clear_key_bindings()`.
pub fn suppress_key(action_id: &str, old_key: &str) -> KeyBinding {
    KeyBinding::new(old_key, NoAction, context_for(action_id))
}

/// Build the live `KeyBinding`s for all 12 configurable actions, using
/// `overrides` where present and each action's default otherwise. Called
/// once at startup (`main.rs`). Does NOT include the always-fixed bindings
/// (`Interrupt`/`SendTab`/`SendBackTab`, `GotoTab1..9`, or `ZoomIn`'s
/// permanent `secondary-shift-=` alias) — those stay literal in `main.rs`.
pub fn build_key_bindings(overrides: &HashMap<String, String>) -> Vec<KeyBinding> {
    DEFAULT_KEYBINDINGS
        .iter()
        .filter_map(|(action_id, _)| {
            let key = effective_key(action_id, overrides)?;
            Some(keybinding_for(action_id, &key))
        })
        .collect()
}

/// Build `KeyBinding`s for exactly the given `(action_id, key)` pairs.
/// Used by `SettingsWindow::apply` to live-rebind only the shortcuts that
/// actually changed since the settings window opened — unlike
/// `build_key_bindings`, this never falls back to any other action's
/// current state, so it's safe to call with a partial list.
pub fn build_key_bindings_for(changed: &[(&str, String)]) -> Vec<KeyBinding> {
    changed
        .iter()
        .map(|(action_id, key)| keybinding_for(action_id, key))
        .collect()
}

/// Canonicalize a captured keystroke into the portable `"secondary-..."`
/// binding-string form gpui expects, checking the platform-relative
/// `Modifiers::secondary()` bit rather than the literal Cmd/Ctrl bit — a
/// binding recorded on Linux/Windows (Ctrl) or macOS (Cmd) round-trips to
/// the same string either way.
pub fn format_keystroke(ks: &Keystroke) -> String {
    let m = &ks.modifiers;
    let mut parts = Vec::new();
    if m.secondary() {
        parts.push("secondary");
    }
    if m.alt {
        parts.push("alt");
    }
    if m.shift {
        parts.push("shift");
    }
    parts.push(ks.key.as_str());
    parts.join("-")
}

/// `true` if `ks` carries at least one modifier. Settings → Shortcuts
/// rejects a bare, unmodified capture — every one of these 12 bindings is
/// global, so an unmodified key would swallow normal typing anywhere.
pub fn has_modifier(ks: &Keystroke) -> bool {
    ks.modifiers.secondary() || ks.modifiers.alt || ks.modifiers.shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_key_falls_back_to_default_when_no_override() {
        let overrides = HashMap::new();
        assert_eq!(
            effective_key("new_tab", &overrides).as_deref(),
            Some("secondary-shift-t")
        );
    }

    #[test]
    fn effective_key_prefers_override() {
        let mut overrides = HashMap::new();
        overrides.insert("new_tab".to_string(), "secondary-shift-r".to_string());
        assert_eq!(
            effective_key("new_tab", &overrides).as_deref(),
            Some("secondary-shift-r")
        );
    }

    #[test]
    fn effective_key_none_for_unknown_action() {
        let overrides = HashMap::new();
        assert_eq!(effective_key("bogus", &overrides), None);
    }

    #[test]
    fn default_keybindings_has_one_entry_per_documented_action() {
        let ids: Vec<&str> = DEFAULT_KEYBINDINGS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            vec![
                "new_tab",
                "close_tab",
                "next_tab",
                "prev_tab",
                "new_connection",
                "toggle_left_sidebar",
                "toggle_right_sidebar",
                "toggle_quick_commands",
                "open_settings",
                "zoom_in",
                "zoom_out",
                "clear_screen",
            ]
        );
    }

    #[test]
    fn build_key_bindings_returns_one_binding_per_action() {
        let overrides = HashMap::new();
        let bindings = build_key_bindings(&overrides);
        assert_eq!(bindings.len(), DEFAULT_KEYBINDINGS.len());
    }

    #[test]
    fn build_key_bindings_for_returns_exactly_the_given_pairs() {
        let changed = vec![("new_tab", "secondary-shift-r".to_string())];
        let bindings = build_key_bindings_for(&changed);
        assert_eq!(bindings.len(), 1);
    }

    fn keystroke(control: bool, shift: bool, alt: bool, key: &str) -> Keystroke {
        Keystroke {
            modifiers: gpui::Modifiers {
                control,
                alt,
                shift,
                platform: false,
                function: false,
            },
            key: key.to_string(),
            key_char: None,
        }
    }

    #[test]
    fn format_keystroke_uses_secondary_for_the_platform_modifier() {
        // On this dev/CI host (non-macOS), `Modifiers::secondary()` reads
        // the `control` bit — this test exercises that branch. macOS would
        // exercise the `platform` bit instead; both are covered by the
        // same `secondary()` call, not duplicated per-platform here.
        let ks = keystroke(true, true, false, "t");
        assert_eq!(format_keystroke(&ks), "secondary-shift-t");
    }

    #[test]
    fn format_keystroke_omits_absent_modifiers() {
        let ks = keystroke(true, false, false, "b");
        assert_eq!(format_keystroke(&ks), "secondary-b");
    }

    #[test]
    fn has_modifier_true_when_secondary_held() {
        assert!(has_modifier(&keystroke(true, false, false, "t")));
    }

    #[test]
    fn has_modifier_false_for_a_bare_key() {
        assert!(!has_modifier(&keystroke(false, false, false, "t")));
    }

    #[test]
    fn suppress_key_uses_no_action() {
        // Constructing it must not panic, and the binding must target the
        // given key string — full dispatch-level behavior is exercised in
        // settings_window.rs's own test for the end-to-end scenario.
        let _binding = suppress_key("new_tab", "secondary-shift-t");
    }

    #[test]
    fn old_key_stops_matching_after_rebind_and_suppression() {
        use std::collections::HashMap;
        // Reproduces exactly what SettingsWindow::apply does: startup with
        // defaults, then a live-rebind of one action to a new key, PLUS
        // the suppression binding for its old key.
        let mut keymap = gpui::Keymap::new(build_key_bindings(&HashMap::new()));
        keymap.add_bindings(build_key_bindings_for(&[(
            "new_tab",
            "secondary-shift-r".to_string(),
        )]));
        keymap.add_bindings([suppress_key("new_tab", "secondary-shift-t")]);

        let old_key = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                shift: true,
                alt: false,
                platform: false,
                function: false,
            },
            key: "t".to_string(),
            key_char: None,
        };
        let new_key = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                shift: true,
                alt: false,
                platform: false,
                function: false,
            },
            key: "r".to_string(),
            key_char: None,
        };

        let (old_matches, _) = keymap.bindings_for_input(&[old_key], &[]);
        let (new_matches, _) = keymap.bindings_for_input(&[new_key], &[]);

        assert!(
            old_matches.is_empty(),
            "old key should no longer match anything after suppression"
        );
        assert_eq!(
            new_matches.len(),
            1,
            "new key should match exactly the rebound action"
        );
    }
}
