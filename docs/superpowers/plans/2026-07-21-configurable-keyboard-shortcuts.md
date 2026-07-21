# Configurable Keyboard Shortcuts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a "Shortcuts" tab to Caracal's Settings window where the 12
user-configurable application-level keyboard shortcuts can be viewed,
re-recorded, reset, and applied live — no restart required.

**Architecture:** A new registry module (`src/panels/keybindings.rs`) is
the single source of truth mapping a stable action-id string to its
default binding and concrete `gpui::Action` type, replacing the literal
`KeyBinding::new(...)` calls in `main.rs` for the 12 configurable actions.
Persistence is a new `HashMap<String, String>` field on `AppSettings`
holding only the user's *overrides* (unset = default). Live rebinding
never calls `clear_key_bindings()` — it relies on gpui's own documented
precedence rule that a later-registered binding shadows an earlier one for
the same keystroke+context, so Settings → Apply just appends fresh
`KeyBinding`s for whatever changed, never touching gpui-component's own
internal keymap.

**Tech Stack:** Rust, `gpui`, `gpui-component`, `toml`/`serde` (existing
settings persistence), `rust_i18n`.

## Global Constraints

- Scope is exactly the 12 shortcuts listed below — not gpui-component's
  own bindings, not `Interrupt`/`SendTab`/`SendBackTab` (raw terminal
  input), not the 9 `secondary-1`..`secondary-9` jump-to-tab bindings
  (shown as one fixed reference row, never individually editable).
- Every binding string uses gpui's `"secondary-x"` cross-platform modifier
  alias, same as the prior feature.
- Live rebinding is implemented via `cx.bind_keys([...])` appends only —
  `cx.clear_key_bindings()` must never be called anywhere in this feature
  (it would wipe gpui-component's own internal keymap; see the design
  spec's investigation for why).
- Settings → Shortcuts follows the existing draft+Apply/Cancel/Confirm
  model already used by every other tab in `SettingsWindow` — a capture
  only stages into `self.draft`, nothing is written to disk or bound live
  until Apply/Confirm runs.
- Conflict handling: Apply is blocked (inline error, matching the existing
  font-size/scrollback validation pattern) if two of the 12 actions would
  resolve to the same key. Not checked against gpui-component's own
  bindings or the fixed jump-to-tab/zoom-alias keys — out of scope by
  design.
- No screenshot-driven GUI verification — manual testing only, per this
  project's standing convention.

The 12 action-ids, their default keys, and their concrete `Action` types
(all confirmed against the current `main.rs`):

| action-id | default key | Action type | context |
|---|---|---|---|
| `new_tab` | `secondary-shift-t` | `crate::workspace::NewTab` | `None` |
| `close_tab` | `secondary-shift-w` | `gpui_component::dock::ClosePanel` | `None` |
| `next_tab` | `secondary-tab` | `crate::workspace::NextTab` | `None` |
| `prev_tab` | `secondary-shift-tab` | `crate::workspace::PrevTab` | `None` |
| `new_connection` | `secondary-shift-n` | `crate::workspace::NewConnectionAction` | `None` |
| `toggle_left_sidebar` | `secondary-b` | `crate::workspace::ToggleLeftSidebar` | `None` |
| `toggle_right_sidebar` | `secondary-shift-b` | `crate::workspace::ToggleRightSidebar` | `None` |
| `toggle_quick_commands` | `secondary-j` | `crate::workspace::ToggleQuickCommands` | `None` |
| `open_settings` | `secondary-,` | `crate::workspace::OpenSettingsAction` | `None` |
| `zoom_in` | `secondary-=` | `crate::workspace::ZoomIn` | `None` |
| `zoom_out` | `secondary--` | `crate::workspace::ZoomOut` | `None` |
| `clear_screen` | `secondary-shift-l` | `crate::terminal::view::ClearScreen` | `Some(TERMINAL_KEY_CONTEXT)` |

Fixed, never-configurable bindings that stay literal in `main.rs`:
`ctrl-c`→`Interrupt`, `tab`→`SendTab`, `shift-tab`→`SendBackTab` (all
`Some(TERMINAL_KEY_CONTEXT)`), `secondary-1`..`secondary-9`→`GotoTab1`..
`GotoTab9` (`None`), and `secondary-shift-=`→`ZoomIn` (`None`, the
permanent shifted-`+` convenience alias — `zoom_in`'s configurable entry
above only ever changes the primary `secondary-=` binding).

---

### Task 1: Keybindings registry module

**Files:**
- Create: `src/panels/keybindings.rs`
- Modify: `src/panels/mod.rs`

**Interfaces:**
- Produces: `pub const DEFAULT_KEYBINDINGS: &'static [(&'static str, &'static str)]`;
  `pub fn effective_key(action_id: &str, overrides: &HashMap<String, String>) -> Option<String>`;
  `pub fn build_key_bindings(overrides: &HashMap<String, String>) -> Vec<gpui::KeyBinding>`
  (resolves *every* configurable action from `overrides`, used at startup);
  `pub fn build_key_bindings_for(changed: &[(&'static str, String)]) -> Vec<gpui::KeyBinding>`
  (builds bindings for exactly the given action-id/key pairs, used by
  `SettingsWindow::apply` to live-rebind only what changed — see Task 7 for
  why a partial-overrides call into `build_key_bindings` would be wrong);
  `pub fn format_keystroke(ks: &gpui::Keystroke) -> String`;
  `pub fn has_modifier(ks: &gpui::Keystroke) -> bool`.
- Consumes: nothing from other tasks in this plan (self-contained; uses
  the already-existing action types from `crate::workspace` and
  `crate::terminal::view`, and `gpui_component::dock::ClosePanel`).

This module lives under `src/panels/` (not top-level alongside
`settings.rs`) because it needs `gpui_component::dock::ClosePanel` — per
`src/panels/mod.rs`'s own doc comment, `panels/` and `workspace.rs` are
the only two places in this codebase allowed to import `gpui_component`;
every other top-level module (`settings.rs`, `config.rs`, `crypto.rs`,
etc.) is plain Rust. `src/panels/icons.rs` is the existing precedent for a
non-panel-shaped utility module living under `panels/` for exactly this
reason.

- [ ] **Step 1: Write the failing tests**

Create `src/panels/keybindings.rs` with just the test module first:

```rust
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

use gpui::{KeyBinding, Keystroke};
use gpui_component::dock::ClosePanel;

use crate::terminal::view::{ClearScreen, TERMINAL_KEY_CONTEXT};
use crate::workspace::{
    NewConnectionAction, NewTab, NextTab, OpenSettingsAction, PrevTab, ToggleLeftSidebar,
    ToggleQuickCommands, ToggleRightSidebar, ZoomIn, ZoomOut,
};

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
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test panels::keybindings::tests`
Expected: FAIL to compile — none of `DEFAULT_KEYBINDINGS`, `effective_key`,
`build_key_bindings`, `build_key_bindings_for`, `format_keystroke`,
`has_modifier` exist yet.

- [ ] **Step 3: Implement the registry**

Add above the `#[cfg(test)]` block in `src/panels/keybindings.rs`:

```rust
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
```

- [ ] **Step 4: Register the module**

In `src/panels/mod.rs`, add (alphabetically, after `icons`):

```rust
pub mod icons;
pub mod keybindings;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test panels::keybindings::tests`
Expected: PASS (11 tests).

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: builds cleanly. Some `dead_code` warnings are expected here
(nothing calls `build_key_bindings`/`build_key_bindings_for` yet — Tasks 3
and 7 wire them in) — this is the same known foundation-task pattern as
earlier features in this codebase; don't treat it as a failure.

- [ ] **Step 7: Commit**

```bash
git add src/panels/keybindings.rs src/panels/mod.rs
git commit -m "feat: add keybindings registry module"
```

---

### Task 2: Persist keybinding overrides in AppSettings

**Files:**
- Modify: `src/settings.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `AppSettings.keybindings: KeybindingsSettings`, where
  `KeybindingsSettings { pub overrides: HashMap<String, String> }` —
  consumed by Task 3 (`main.rs`) and Task 5-7 (`SettingsWindow`).

- [ ] **Step 1: Write the failing test**

Add to `settings.rs`'s existing `#[cfg(test)] mod tests` block (after
`round_trip_preserves_fields`, whose struct literal you'll also need to
update in Step 3 — it currently constructs `AppSettings { general,
appearance, terminal }` with no `..Default::default()`, so adding a 4th
required field to the struct will fail to compile until that literal is
updated too):

```rust
    #[test]
    fn default_keybindings_overrides_is_empty() {
        let settings = AppSettings::default();
        assert!(settings.keybindings.overrides.is_empty());
    }

    #[test]
    fn round_trip_preserves_keybinding_overrides() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("new_tab".to_string(), "secondary-shift-r".to_string());
        let settings = AppSettings {
            keybindings: KeybindingsSettings { overrides },
            ..AppSettings::default()
        };
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
        assert_eq!(
            parsed.keybindings.overrides.get("new_tab").map(String::as_str),
            Some("secondary-shift-r")
        );
    }

    #[test]
    fn old_settings_file_without_keybindings_table_still_deserializes() {
        let toml_text = r#"
            [appearance]
            theme_name = "Default Dark"

            [terminal]
            font_family = "Consolas"
            font_size = 16.0
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert!(settings.keybindings.overrides.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test settings::tests`
Expected: FAIL to compile — `AppSettings` has no field `keybindings`,
`KeybindingsSettings` doesn't exist.

- [ ] **Step 3: Implement**

In `src/settings.rs`, add the field to `AppSettings` (after `terminal`):

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub terminal: TerminalSettings,
    #[serde(default)]
    pub keybindings: KeybindingsSettings,
}
```

Add the new struct (anywhere after `TerminalSettings`'s `Default` impl,
e.g. right before `settings_path()`):

```rust
/// Per-action keyboard-shortcut overrides, editable from Settings →
/// Shortcuts. Only entries the user has actually changed are present —
/// anything absent falls back to
/// `crate::panels::keybindings::DEFAULT_KEYBINDINGS`. Deliberately just a
/// `HashMap`, not one field per action: a future addition to the default
/// table needs no migration here.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeybindingsSettings {
    #[serde(default)]
    pub overrides: std::collections::HashMap<String, String>,
}
```

Update the existing `round_trip_preserves_fields` test's struct literal
(in the `#[cfg(test)] mod tests` block) to add the new field — find:

```rust
        let settings = AppSettings {
            general: GeneralSettings {
                language: "en".to_string(),
            },
            appearance: AppearanceSettings {
                theme_name: "Ayu Light".to_string(),
                font_family: "JetBrains Mono".to_string(),
                font_fallback: "Symbols Nerd Font".to_string(),
            },
            terminal: TerminalSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                monitor_basic_enabled: true,
                monitor_basic_interval_secs: 10,
                scrollback_lines: 20_000,
                font_fallback1: "JetBrains Mono".to_string(),
                font_fallback2: "Symbols Nerd Font".to_string(),
            },
        };
```

and change its closing to add the new field before the final `};`:

```rust
        let settings = AppSettings {
            general: GeneralSettings {
                language: "en".to_string(),
            },
            appearance: AppearanceSettings {
                theme_name: "Ayu Light".to_string(),
                font_family: "JetBrains Mono".to_string(),
                font_fallback: "Symbols Nerd Font".to_string(),
            },
            terminal: TerminalSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                monitor_basic_enabled: true,
                monitor_basic_interval_secs: 10,
                scrollback_lines: 20_000,
                font_fallback1: "JetBrains Mono".to_string(),
                font_fallback2: "Symbols Nerd Font".to_string(),
            },
            keybindings: KeybindingsSettings::default(),
        };
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test settings::tests`
Expected: PASS (all settings tests, including the 3 new ones and the
updated `round_trip_preserves_fields`).

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: no regressions elsewhere.

- [ ] **Step 6: Commit**

```bash
git add src/settings.rs
git commit -m "feat: persist keyboard-shortcut overrides in AppSettings"
```

---

### Task 3: Wire the registry into main.rs startup

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `panels::keybindings::build_key_bindings(&HashMap<String, String>) -> Vec<KeyBinding>` (Task 1), `AppSettings.keybindings.overrides` (Task 2).

- [ ] **Step 1: Replace the 12 configurable literal bindings with the registry call**

In `src/main.rs`, the current `cx.bind_keys([...])` block (around line
158) mixes fixed and configurable bindings. Replace the whole block:

```rust
        cx.bind_keys([
            KeyBinding::new("ctrl-c", Interrupt, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("tab", SendTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("shift-tab", SendBackTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("secondary-shift-l", ClearScreen, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("secondary-shift-t", NewTab, None),
            KeyBinding::new("secondary-shift-w", ClosePanel, None),
            KeyBinding::new("secondary-shift-n", NewConnectionAction, None),
            KeyBinding::new("secondary-tab", NextTab, None),
            KeyBinding::new("secondary-shift-tab", PrevTab, None),
            KeyBinding::new("secondary-1", GotoTab1, None),
            KeyBinding::new("secondary-2", GotoTab2, None),
            KeyBinding::new("secondary-3", GotoTab3, None),
            KeyBinding::new("secondary-4", GotoTab4, None),
            KeyBinding::new("secondary-5", GotoTab5, None),
            KeyBinding::new("secondary-6", GotoTab6, None),
            KeyBinding::new("secondary-7", GotoTab7, None),
            KeyBinding::new("secondary-8", GotoTab8, None),
            KeyBinding::new("secondary-9", GotoTab9, None),
            KeyBinding::new("secondary-b", ToggleLeftSidebar, None),
            KeyBinding::new("secondary-shift-b", ToggleRightSidebar, None),
            KeyBinding::new("secondary-j", ToggleQuickCommands, None),
            KeyBinding::new("secondary-,", OpenSettingsAction, None),
            KeyBinding::new("secondary-=", ZoomIn, None),
            KeyBinding::new("secondary-shift-=", ZoomIn, None),
            KeyBinding::new("secondary--", ZoomOut, None),
        ]);
```

with:

```rust
        // Fixed bindings that are never user-configurable: raw terminal
        // input (Interrupt/SendTab/SendBackTab), the 9 jump-to-tab keys,
        // and ZoomIn's permanent shifted-`+` convenience alias. The 12
        // *configurable* shortcuts (Settings → Shortcuts) come from
        // `panels::keybindings::build_key_bindings`, seeded with whatever
        // the user has overridden in `settings.toml` — see that module's
        // doc comment for why later Settings-window edits never need
        // `clear_key_bindings()`.
        let mut key_bindings = vec![
            KeyBinding::new("ctrl-c", Interrupt, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("tab", SendTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("shift-tab", SendBackTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("secondary-1", GotoTab1, None),
            KeyBinding::new("secondary-2", GotoTab2, None),
            KeyBinding::new("secondary-3", GotoTab3, None),
            KeyBinding::new("secondary-4", GotoTab4, None),
            KeyBinding::new("secondary-5", GotoTab5, None),
            KeyBinding::new("secondary-6", GotoTab6, None),
            KeyBinding::new("secondary-7", GotoTab7, None),
            KeyBinding::new("secondary-8", GotoTab8, None),
            KeyBinding::new("secondary-9", GotoTab9, None),
            KeyBinding::new("secondary-shift-=", ZoomIn, None),
        ];
        key_bindings.extend(panels::keybindings::build_key_bindings(
            &startup_settings.keybindings.overrides,
        ));
        cx.bind_keys(key_bindings);
```

Note this new block reads `startup_settings` (already bound above it via
`let startup_settings = settings::load();` at the top of the closure) —
no new `settings::load()` call needed. Remove the now-unused imports this
leaves behind: `ClearScreen` (from the `terminal::view::{...}` import),
and from the `workspace::{...}` import: `NewTab`, `NewConnectionAction`,
`NextTab`, `PrevTab`, `ToggleLeftSidebar`, `ToggleQuickCommands`,
`ToggleRightSidebar`, `OpenSettingsAction`, `ZoomOut` (keep `ZoomIn` — the
fixed alias binding above still needs it), and the top-level
`gpui_component::dock::ClosePanel` import. The import lines become:

```rust
use terminal::view::{Interrupt, SendBackTab, SendTab, TERMINAL_KEY_CONTEXT};
use workspace::{
    GotoTab1, GotoTab2, GotoTab3, GotoTab4, GotoTab5, GotoTab6, GotoTab7, GotoTab8, GotoTab9,
    Workspace, ZoomIn,
};
```

(the `use gpui_component::dock::ClosePanel;` line is deleted entirely).

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: clean build, no unused-import warnings, no unused-`dead_code`
warnings for `build_key_bindings` (now called).

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: no regressions.

- [ ] **Step 4: Manual smoke test**

Run: `cargo run`. Confirm the app launches and a couple of the 12
shortcuts still work as before (e.g. `secondary-shift-t` opens a new tab,
`secondary-,` opens Settings) — this task only changes *how* the bindings
are registered, not their default values, so behavior should be
byte-for-byte identical to before this task.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: source configurable keybindings from the registry at startup"
```

---

### Task 4: Locale entries

**Files:**
- Modify: `locales/app.yml`

**Interfaces:**
- Produces: locale keys under `Settings.Shortcuts.*`, consumed by Task 5
  (row/tab labels) and Task 6-7 (capture/error messages).

- [ ] **Step 1: Add the new locale entries**

In `locales/app.yml`, inside the existing `Settings:` top-level block
(after the `Security:` sub-block, before the `theme_label:` key — i.e. as
a sibling sub-namespace to `General:`/`Security:`), add:

```yaml
  Shortcuts:
    new_tab:
      zh-CN: "新建标签页"
      en: "New Tab"
    close_tab:
      zh-CN: "关闭标签页"
      en: "Close Tab"
    next_tab:
      zh-CN: "下一个标签页"
      en: "Next Tab"
    prev_tab:
      zh-CN: "上一个标签页"
      en: "Previous Tab"
    new_connection:
      zh-CN: "新建连接"
      en: "New Connection"
    toggle_left_sidebar:
      zh-CN: "切换左侧栏"
      en: "Toggle Left Sidebar"
    toggle_right_sidebar:
      zh-CN: "切换右侧栏"
      en: "Toggle Right Sidebar"
    toggle_quick_commands:
      zh-CN: "切换快捷命令面板"
      en: "Toggle Quick Commands"
    open_settings:
      zh-CN: "打开设置"
      en: "Open Settings"
    zoom_in:
      zh-CN: "放大字体"
      en: "Zoom In"
    zoom_out:
      zh-CN: "缩小字体"
      en: "Zoom Out"
    clear_screen:
      zh-CN: "清除屏幕"
      en: "Clear Screen"
    goto_tab_label:
      zh-CN: "跳转到标签页 1-9"
      en: "Jump to Tab 1-9"
    record_button:
      zh-CN: "记录"
      en: "Record"
    recording_prompt:
      zh-CN: "按下新按键…"
      en: "Press a key…"
    reset_button:
      zh-CN: "恢复默认"
      en: "Reset"
    reset_all_button:
      zh-CN: "全部恢复默认"
      en: "Reset All"
    needs_modifier:
      zh-CN: "需要至少一个修饰键"
      en: "Requires at least one modifier key"
    conflict_error:
      zh-CN: "%{key} 已被“%{action}”占用"
      en: "%{key} is already used by \"%{action}\""
```

- [ ] **Step 2: Verify the YAML is well-formed and the app still builds**

Run: `cargo build`
Expected: clean build (a malformed `locales/app.yml` fails the
`rust_i18n::i18n!` macro at compile time, so a successful build already
confirms the YAML parses).

- [ ] **Step 3: Commit**

```bash
git add locales/app.yml
git commit -m "feat: add locale entries for the Shortcuts settings tab"
```

---

### Task 5: Shortcuts tab — scaffold, display, reset

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `panels::keybindings::{DEFAULT_KEYBINDINGS, effective_key}`
  (Task 1), `AppSettings.keybindings.overrides` (Task 2), the
  `Settings.Shortcuts.*` locale keys (Task 4).
- Produces: `SettingsTab::Shortcuts` variant — consumed by Task 6/7's
  additions to the same file. `fn shortcut_action_label(action_id: &str) -> SharedString`
  — reused by Task 7's conflict-error message.

No capture/recording UI yet — this task only makes the tab exist, show
every action's current key (override or default), and let Reset/Reset-All
stage changes into the draft. Task 6 adds the interactive "Record" flow.

- [ ] **Step 1: Add the new tab variant**

In `src/panels/settings_window.rs`, extend `SettingsTab`:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Appearance,
    Terminal,
    Security,
    Shortcuts,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Terminal => "Terminal",
            SettingsTab::Security => "Security",
            SettingsTab::Shortcuts => "Shortcuts",
        }
    }
}
```

(matches the existing convention exactly: these sidebar labels are plain
`&'static str`, not run through `rust_i18n::t!` — every other tab label in
this file is unlocalized today too, so this isn't a new inconsistency.)

- [ ] **Step 2: Wire the sidebar button and content dispatch**

In `Render for SettingsWindow`'s `render` method, extend the `match`:

```rust
        let content = match self.active_tab {
            SettingsTab::General => self.render_general_tab(cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_tab(cx).into_any_element(),
            SettingsTab::Terminal => self.render_terminal_tab(cx).into_any_element(),
            SettingsTab::Security => self.render_security_tab(cx).into_any_element(),
            SettingsTab::Shortcuts => self.render_shortcuts_tab(cx).into_any_element(),
        };
```

and the sidebar's tab-button list:

```rust
                            .child(self.tab_button(SettingsTab::General, cx))
                            .child(self.tab_button(SettingsTab::Appearance, cx))
                            .child(self.tab_button(SettingsTab::Terminal, cx))
                            .child(self.tab_button(SettingsTab::Security, cx))
                            .child(self.tab_button(SettingsTab::Shortcuts, cx)),
```

- [ ] **Step 3: Add the action-label helper**

Add near the top of the file (after `font_choice_label`, which follows
the same "map a stable id to a localized label" shape):

```rust
/// Localized display label for one of the 12 configurable shortcut
/// action-ids (see `panels::keybindings::DEFAULT_KEYBINDINGS`). Falls back
/// to the raw id for anything unrecognized (shouldn't happen — every
/// caller sources ids from that same table).
fn shortcut_action_label(action_id: &str) -> SharedString {
    match action_id {
        "new_tab" => rust_i18n::t!("Settings.Shortcuts.new_tab").into(),
        "close_tab" => rust_i18n::t!("Settings.Shortcuts.close_tab").into(),
        "next_tab" => rust_i18n::t!("Settings.Shortcuts.next_tab").into(),
        "prev_tab" => rust_i18n::t!("Settings.Shortcuts.prev_tab").into(),
        "new_connection" => rust_i18n::t!("Settings.Shortcuts.new_connection").into(),
        "toggle_left_sidebar" => rust_i18n::t!("Settings.Shortcuts.toggle_left_sidebar").into(),
        "toggle_right_sidebar" => rust_i18n::t!("Settings.Shortcuts.toggle_right_sidebar").into(),
        "toggle_quick_commands" => rust_i18n::t!("Settings.Shortcuts.toggle_quick_commands").into(),
        "open_settings" => rust_i18n::t!("Settings.Shortcuts.open_settings").into(),
        "zoom_in" => rust_i18n::t!("Settings.Shortcuts.zoom_in").into(),
        "zoom_out" => rust_i18n::t!("Settings.Shortcuts.zoom_out").into(),
        "clear_screen" => rust_i18n::t!("Settings.Shortcuts.clear_screen").into(),
        _ => SharedString::from(action_id.to_string()),
    }
}
```

- [ ] **Step 4: Add the tab-content renderer and its row helper**

Add near `render_security_tab`:

```rust
    /// One row: action label, current resolved key (override or
    /// default), and a Reset button. `Record` (interactive capture) is
    /// added in a later change to this same function.
    fn shortcut_row(&self, action_id: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let current_key =
            keybindings::effective_key(action_id, &self.draft.keybindings.overrides)
                .unwrap_or_default();

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().text_sm().child(shortcut_action_label(action_id)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(current_key),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "settings-shortcut-reset-{action_id}"
                        )))
                        .xsmall()
                        .label(rust_i18n::t!("Settings.Shortcuts.reset_button"))
                        .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                            this.reset_shortcut(action_id, cx);
                        })),
                    ),
            )
    }

    fn reset_shortcut(&mut self, action_id: &'static str, cx: &mut Context<Self>) {
        self.draft.keybindings.overrides.remove(action_id);
        cx.notify();
    }

    fn on_click_reset_all_shortcuts(
        &mut self,
        _ev: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft.keybindings.overrides.clear();
        cx.notify();
    }

    fn render_shortcuts_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let mut rows = div().flex().flex_col().gap_2();
        for (action_id, _) in keybindings::DEFAULT_KEYBINDINGS {
            rows = rows.child(self.shortcut_row(action_id, cx));
        }

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(rows)
            .child(
                div()
                    .pt_2()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{}: secondary-1 .. secondary-9",
                        rust_i18n::t!("Settings.Shortcuts.goto_tab_label")
                    )),
            )
            .child(
                Button::new("settings-shortcuts-reset-all")
                    .xsmall()
                    .label(rust_i18n::t!("Settings.Shortcuts.reset_all_button"))
                    .on_click(cx.listener(Self::on_click_reset_all_shortcuts)),
            )
    }
```

- [ ] **Step 5: Add the missing import**

`src/panels/settings_window.rs` needs `crate::panels::keybindings` in
scope. Add to its `use crate::...` block:

```rust
use crate::panels::keybindings;
```

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 7: Manual test**

Run: `cargo run`, open Settings, click the new "Shortcuts" tab. Confirm
all 12 rows show their correct default key, plus the read-only jump-to-tab
reference line. Click a row's Reset — no visible change yet (it was
already at default), but click Reset All — no errors, no panic. Cancel out
without Apply.

- [ ] **Step 8: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add read-only Shortcuts settings tab with reset"
```

---

### Task 6: Interactive shortcut capture

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `panels::keybindings::{format_keystroke, has_modifier}` (Task 1).
- Produces: `SettingsWindow.recording: Option<String>`,
  `SettingsWindow.record_focus: FocusHandle` — read by Task 7's Apply
  integration only indirectly (via the already-staged `self.draft.keybindings.overrides`, not these fields directly).

- [ ] **Step 1: Add capture state to `SettingsWindow`**

Add two fields to the `SettingsWindow` struct:

```rust
pub struct SettingsWindow {
    workspace: WeakEntity<Workspace>,
    committed: AppSettings,
    draft: AppSettings,
    active_tab: SettingsTab,
    font_size_input: Entity<InputState>,
    monitor_interval_input: Entity<InputState>,
    scrollback_input: Entity<InputState>,
    error: Option<SharedString>,
    /// The action-id currently being recorded (`Record` button clicked,
    /// waiting for the next keystroke), or `None` when nothing is being
    /// recorded.
    recording: Option<String>,
    /// Focus target for capturing the next keystroke while recording —
    /// created once in `new`, (re)focused via `window.focus(...)` each
    /// time `Record` is clicked.
    record_focus: FocusHandle,
}
```

and initialize both in `SettingsWindow::new`'s `Self { ... }`:

```rust
        Self {
            workspace,
            draft: committed.clone(),
            committed,
            active_tab: SettingsTab::Appearance,
            font_size_input,
            monitor_interval_input,
            scrollback_input,
            error: None,
            recording: None,
            record_focus: cx.focus_handle(),
        }
```

- [ ] **Step 2: Add the capture handlers**

Add near `reset_shortcut`:

```rust
    fn start_recording(
        &mut self,
        action_id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.recording = Some(action_id.to_string());
        self.error = None;
        window.focus(&self.record_focus, cx);
        cx.notify();
    }

    /// Handles the next keystroke while `self.recording` is `Some`.
    /// Escape cancels (reverts to whatever key the row had before,
    /// without capturing Escape itself as a binding — standard capture-
    /// widget convention). A bare, unmodified key is rejected inline. A
    /// successful capture stages the canonicalized key string into
    /// `self.draft.keybindings.overrides` — nothing is bound live or
    /// saved to disk here; that only happens on Apply (Task 7).
    fn on_capture_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(action_id) = self.recording.clone() else {
            return;
        };
        let ks = &ev.keystroke;
        if ks.key == "escape" {
            self.recording = None;
            cx.notify();
            return;
        }
        if !keybindings::has_modifier(ks) {
            self.error = Some(rust_i18n::t!("Settings.Shortcuts.needs_modifier").into());
            cx.notify();
            return;
        }
        let key_string = keybindings::format_keystroke(ks);
        self.draft.keybindings.overrides.insert(action_id, key_string);
        self.recording = None;
        self.error = None;
        cx.notify();
    }
```

- [ ] **Step 3: Add the Record button and wire the capture element**

Replace `shortcut_row` (from Task 5) with this version, which adds the
Record button and the recording-state label:

```rust
    fn shortcut_row(&self, action_id: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let current_key =
            keybindings::effective_key(action_id, &self.draft.keybindings.overrides)
                .unwrap_or_default();
        let is_recording = self.recording.as_deref() == Some(action_id);

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().text_sm().child(shortcut_action_label(action_id)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if is_recording {
                                rust_i18n::t!("Settings.Shortcuts.recording_prompt").to_string()
                            } else {
                                current_key
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "settings-shortcut-record-{action_id}"
                        )))
                        .xsmall()
                        .label(rust_i18n::t!("Settings.Shortcuts.record_button"))
                        .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                            this.start_recording(action_id, window, cx);
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "settings-shortcut-reset-{action_id}"
                        )))
                        .xsmall()
                        .label(rust_i18n::t!("Settings.Shortcuts.reset_button"))
                        .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                            this.reset_shortcut(action_id, cx);
                        })),
                    ),
            )
    }
```

Wrap `render_shortcuts_tab`'s outer element with the focus-tracking
capture div — change its opening `div()` chain from:

```rust
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(rows)
```

to:

```rust
        div()
            .track_focus(&self.record_focus)
            .on_key_down(cx.listener(Self::on_capture_key_down))
            .flex()
            .flex_col()
            .gap_3()
            .child(rows)
```

(the rest of `render_shortcuts_tab` — the goto-tab reference line and the
Reset All button — stays exactly as Task 5 wrote it.)

- [ ] **Step 4: Add the missing imports**

Extend the `use gpui::{...}` block in `src/panels/settings_window.rs` to
include `FocusHandle` and `KeyDownEvent`:

```rust
use gpui::{
    App, AppContext, ClickEvent, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, prelude::FluentBuilder, px, red, transparent_black,
};
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 6: Manual test**

Run: `cargo run`, open Settings → Shortcuts. Click "记录"/"Record" on any
row: its key text changes to "按下新按键…"/"Press a key…". Press a bare
letter (no modifier): inline error appears, still recording. Press
Escape: reverts to the prior key, recording stops. Click Record again,
press a real modified combo (e.g. `Ctrl+Shift+R` / `Cmd+Shift+R`): the row
updates to show the new key. Cancel the window — reopen Settings and
confirm the change did NOT persist (capture only stages into the draft;
Apply/Confirm isn't wired until Task 7).

- [ ] **Step 7: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add interactive shortcut capture to the Shortcuts tab"
```

---

### Task 7: Conflict validation and live-rebind on Apply

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `panels::keybindings::{DEFAULT_KEYBINDINGS, effective_key, build_key_bindings_for}` (Task 1), `shortcut_action_label` (Task 5).
- Produces: `fn find_keybinding_conflict(overrides: &HashMap<String, String>) -> Option<(String, &'static str, &'static str)>` — free function, unit tested here, not consumed elsewhere.

This is the task that actually makes Shortcuts changes take effect: on
Apply, block on a same-key conflict (matching the existing font-size/
scrollback-lines inline-error pattern), otherwise live-rebind exactly the
actions whose *resolved* key changed and persist as usual.

- [ ] **Step 1: Write the failing tests**

Add to `settings_window.rs`'s `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn find_keybinding_conflict_none_when_all_defaults() {
        let overrides = std::collections::HashMap::new();
        assert!(find_keybinding_conflict(&overrides).is_none());
    }

    #[test]
    fn find_keybinding_conflict_detects_a_collision() {
        let mut overrides = std::collections::HashMap::new();
        // toggle_left_sidebar's default is "secondary-b" — collide new_tab into it.
        overrides.insert("new_tab".to_string(), "secondary-b".to_string());
        let conflict = find_keybinding_conflict(&overrides).expect("expected a conflict");
        assert_eq!(conflict.0, "secondary-b");
        assert_eq!(conflict.1, "new_tab");
        assert_eq!(conflict.2, "toggle_left_sidebar");
    }

    #[test]
    fn find_keybinding_conflict_none_when_override_is_still_unique() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("new_tab".to_string(), "secondary-shift-r".to_string());
        assert!(find_keybinding_conflict(&overrides).is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test panels::settings_window::tests`
Expected: FAIL to compile — `find_keybinding_conflict` doesn't exist.

- [ ] **Step 3: Implement the conflict check**

Add near `parse_scrollback_lines` (the other small pure validation
functions at the top of the file):

```rust
/// Check the 12 configurable shortcuts' *resolved* keys (override or
/// default, via `keybindings::effective_key`) for a duplicate. Returns
/// `(key, first_action_id, second_action_id)` for the first collision
/// found (table order), or `None` if every resolved key is unique. O(n^2)
/// over exactly 12 entries — a HashMap-based dedup isn't worth the extra
/// code at this fixed, tiny size.
fn find_keybinding_conflict(
    overrides: &std::collections::HashMap<String, String>,
) -> Option<(String, &'static str, &'static str)> {
    let resolved: Vec<(&'static str, String)> = keybindings::DEFAULT_KEYBINDINGS
        .iter()
        .map(|(id, _)| (*id, keybindings::effective_key(id, overrides).unwrap_or_default()))
        .collect();
    for i in 0..resolved.len() {
        for j in (i + 1)..resolved.len() {
            if resolved[i].1 == resolved[j].1 {
                return Some((resolved[i].1.clone(), resolved[i].0, resolved[j].0));
            }
        }
    }
    None
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test panels::settings_window::tests`
Expected: PASS (all settings_window tests, including the 3 new ones).

- [ ] **Step 5: Wire the conflict check and live-rebind into `apply`**

In `SettingsWindow::apply`, add the conflict check right after
`sync_inputs_to_draft` succeeds (before `settings::save`):

```rust
    fn apply(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.sync_inputs_to_draft(cx) {
            cx.notify();
            return false;
        }

        if let Some((key, _first, second)) =
            find_keybinding_conflict(&self.draft.keybindings.overrides)
        {
            self.error = Some(
                rust_i18n::t!(
                    "Settings.Shortcuts.conflict_error",
                    key = key,
                    action = shortcut_action_label(second)
                )
                .into(),
            );
            cx.notify();
            return false;
        }

        if let Err(e) = settings::save(&self.draft) {
```

(the rest of `apply` — theme/locale/font application — stays unchanged,
up through the existing `self.committed = self.draft.clone(); cx.notify();
true` at its end). Add the live-rebind step at the very end, right before
that final `self.committed = self.draft.clone();` line:

```rust
        // Keyboard shortcuts: re-register only the actions whose resolved
        // key actually changed since `committed` — appending a fresh
        // `KeyBinding` is immediate and safe (see `keybindings.rs`'s doc
        // comment: gpui's own precedence rule means a later-added binding
        // shadows an earlier one for the same keystroke+context, so this
        // never needs `clear_key_bindings()` and never touches
        // gpui-component's own internal bindings).
        let mut changed: Vec<(&'static str, String)> = Vec::new();
        for (action_id, _) in keybindings::DEFAULT_KEYBINDINGS {
            let old_key =
                keybindings::effective_key(action_id, &self.committed.keybindings.overrides);
            let new_key =
                keybindings::effective_key(action_id, &self.draft.keybindings.overrides);
            if old_key != new_key {
                if let Some(new_key) = new_key {
                    changed.push((action_id, new_key));
                }
            }
        }
        if !changed.is_empty() {
            cx.bind_keys(keybindings::build_key_bindings_for(&changed));
        }

        self.committed = self.draft.clone();
        cx.notify();
        true
    }
```

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 7: Run the full test suite**

Run: `cargo test`
Expected: no regressions; new tests pass.

- [ ] **Step 8: Manual test**

Run: `cargo run`, open Settings → Shortcuts:
- Record a new key for "New Tab" (e.g. `Ctrl+Shift+R`), click Apply.
  Close Settings entirely, reopen it — the new key is still shown
  (persisted). Press the new key combo somewhere in the app: it opens a
  new tab. Press the *old* default (`Ctrl+Shift+T`): confirm it does
  **not** also still work (only the new key is live — this is the
  "shadowing" the design relies on, worth confirming by hand since it's
  the one behavior no automated test in this plan directly exercises).
- Try to record "Close Tab" using the current key for "Toggle Left
  Sidebar" (`Ctrl+B` by default, unless already changed above): clicking
  Apply should show the inline conflict error and the window should stay
  open with nothing saved.
- Click "全部恢复默认"/"Reset All", then Apply: confirms every shortcut
  reverts to its default and still works live.

- [ ] **Step 9: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: live-rebind and conflict-check shortcuts on Settings apply"
```

---

## Self-Review Notes

- **Spec coverage:** registry module (Task 1), persistence (Task 2),
  startup wiring (Task 3), locale (Task 4), read-only display + reset
  (Task 5), interactive capture (Task 6), conflict validation + live
  rebind on Apply (Task 7) — every section of the design spec maps to a
  task. The "never call `clear_key_bindings()`" constraint is satisfied
  structurally (no task ever calls it) and documented in three places
  (spec, plan header, `keybindings.rs`'s own doc comment) so a future
  editor doesn't reintroduce it.
- **Placeholder scan:** no TBDs; every step has literal code.
- **Type consistency:** `action_id: &'static str` is used consistently
  from `DEFAULT_KEYBINDINGS` through `shortcut_row`/`start_recording`/
  `reset_shortcut`'s signatures; `HashMap<String, String>` is the type of
  `overrides` everywhere (Task 2's `KeybindingsSettings.overrides`, Task
  1's `effective_key`/`build_key_bindings`, Task 7's
  `find_keybinding_conflict`) — no `HashMap<&str, &str>`/owned-vs-borrowed
  mismatches between tasks.
